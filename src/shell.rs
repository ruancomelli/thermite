use std::path::Path;
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, info};

use crate::error::{Result, ThermiteError};

/// Global verbosity level for this process.
///
/// | Level | Flag  | Effect                                                   |
/// |-------|-------|----------------------------------------------------------|
/// | 0     | (none)| Normal output only                                       |
/// | 1     | `-v`  | Print each external command before it is executed        |
/// | 2     | `-vv` | Level 1 + per-phase explanations with documentation links|
///
/// Set once at startup via [`set_verbosity`]; read anywhere via
/// [`verbosity`] or the convenience helper [`is_verbose`].
static VERBOSITY: OnceLock<u8> = OnceLock::new();

/// Global output sink for the TUI.
///
/// When set, every line that would normally be printed to stdout/stderr
/// (external command output, `verbose` command echoes, `ui` helpers, etc.)
/// is sent to this channel instead.  The TUI event loop consumes the channel
/// and renders the lines in its log pane, so the alternate screen is never
/// corrupted by stray writes.
///
/// Set via [`set_output_sink`] before entering the TUI and cleared via
/// [`clear_output_sink`] on exit.  When unset, all output goes to the real
/// terminal as before.
static OUTPUT_SINK: Mutex<Option<UnboundedSender<String>>> = Mutex::new(None);

/// Route all process output into the given channel.
pub fn set_output_sink(tx: UnboundedSender<String>) {
    *OUTPUT_SINK.lock().expect("OUTPUT_SINK mutex poisoned") = Some(tx);
}

/// Remove the output sink, restoring direct terminal output.
pub fn clear_output_sink() {
    *OUTPUT_SINK.lock().expect("OUTPUT_SINK mutex poisoned") = None;
}

/// Whether a TUI output sink is currently active.
pub fn is_sink_active() -> bool {
    OUTPUT_SINK
        .lock()
        .expect("OUTPUT_SINK mutex poisoned")
        .is_some()
}

/// Emit one line of process output.
///
/// Lines go to the output sink when a TUI is running, otherwise straight to
/// stdout.  All user-facing printing in code that may run under the TUI must
/// go through this helper (or a `ui::print_*` helper, which uses it) rather
/// than calling `println!` directly, so that the TUI's alternate screen is
/// never corrupted.
pub fn out_line(line: &str) {
    if let Some(tx) = OUTPUT_SINK
        .lock()
        .expect("OUTPUT_SINK mutex poisoned")
        .as_ref()
    {
        let _ = tx.send(line.to_owned());
    } else {
        println!("{line}");
    }
}

/// Set the process-wide verbosity level.
///
/// Must be called before any commands are run.  Subsequent calls are silently
/// ignored (the level is immutable after the first call).
pub fn set_verbosity(level: u8) {
    let _ = VERBOSITY.set(level);
}

/// Returns the current verbosity level (0 = quiet, 1 = `-v`, 2 = `-vv`).
pub fn verbosity() -> u8 {
    VERBOSITY.get().copied().unwrap_or(0)
}

/// Returns `true` if at least one `-v` flag was given (`verbosity >= 1`).
pub fn is_verbose() -> bool {
    verbosity() >= 1
}

/// Output captured from a completed external command.
#[derive(Debug)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub status: std::process::ExitStatus,
}

/// Run an external command, streaming its output to the terminal in real time
/// and collecting it into a [`CommandOutput`].
///
/// `env` entries are added to the child process environment on top of the
/// current process's environment.
pub async fn run_command(
    program: &str,
    args: &[&str],
    cwd: &Path,
    env: &[(&str, &str)],
) -> Result<CommandOutput> {
    let display_cmd = format!("{} {}", program, args.join(" "));
    debug!("running: {display_cmd}");
    if is_verbose() {
        out_line(&format!("+ {display_cmd}"));
    }

    // Check the program exists before spawning so we surface a clear error
    // rather than a cryptic OS error.
    //
    // Programs that contain '/' are relative or absolute path references
    // (e.g. "debian/rules"), not PATH lookups; verify them as files relative
    // to `cwd` rather than searching PATH.
    if program.contains('/') {
        let full_path = cwd.join(program);
        if !full_path.exists() {
            return Err(ThermiteError::CommandNotFound(program.to_owned()));
        }
    } else {
        which(program)?;
    }

    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for (k, v) in env {
        cmd.env(k, v);
    }

    // Finding 14 (clippy): use the variant directly instead of a redundant closure.
    let mut child = cmd.spawn().map_err(ThermiteError::Io)?;

    // Finding 11: replace expect() with recoverable errors so a misconfiguration
    // returns a structured error rather than aborting the process.
    let stdout_handle = child.stdout.take().ok_or_else(|| {
        ThermiteError::Io(std::io::Error::other(
            "stdout was not piped — this is a bug in run_command",
        ))
    })?;
    let stderr_handle = child.stderr.take().ok_or_else(|| {
        ThermiteError::Io(std::io::Error::other(
            "stderr was not piped — this is a bug in run_command",
        ))
    })?;

    let mut stdout_buf = String::new();
    let mut stderr_buf = String::new();

    let mut stdout_reader = BufReader::new(stdout_handle).lines();
    let mut stderr_reader = BufReader::new(stderr_handle).lines();

    loop {
        tokio::select! {
            line = stdout_reader.next_line() => {
                match line.map_err(ThermiteError::Io)? {
                    Some(l) => {
                        out_line(&l);
                        stdout_buf.push_str(&l);
                        stdout_buf.push('\n');
                    }
                    None => break,
                }
            }
            line = stderr_reader.next_line() => {
                match line.map_err(ThermiteError::Io)? {
                    Some(l) => {
                        out_line(&l);
                        stderr_buf.push_str(&l);
                        stderr_buf.push('\n');
                    }
                    None => break,
                }
            }
        }
    }

    // Drain any remaining lines after the select loop exits.
    while let Some(l) = stdout_reader.next_line().await.map_err(ThermiteError::Io)? {
        out_line(&l);
        stdout_buf.push_str(&l);
        stdout_buf.push('\n');
    }
    while let Some(l) = stderr_reader.next_line().await.map_err(ThermiteError::Io)? {
        out_line(&l);
        stderr_buf.push_str(&l);
        stderr_buf.push('\n');
    }

    let status = child.wait().await.map_err(ThermiteError::Io)?;
    info!("{display_cmd} exited with {status}");

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        // Finding 1: include captured stdout so callers can parse tool output
        // that was written to stdout on the failure path.
        return Err(ThermiteError::CommandFailed {
            cmd: display_cmd,
            code,
            stdout: stdout_buf,
            stderr: stderr_buf,
        });
    }

    Ok(CommandOutput {
        stdout: stdout_buf,
        stderr: stderr_buf,
        status,
    })
}

/// Run an external command that requires direct terminal access.
///
/// Unlike [`run_command`], this variant inherits stdin, stdout, and stderr
/// from the parent process so that interactive programs (e.g. `git rebase -i`
/// with an editor, or `quilt push -f --merge` with a pager) get a real TTY.
///
/// Finding 4: the piped runner used by `run_command` is not suitable for
/// interactive editor-driven workflows.
pub async fn run_interactive_command(
    program: &str,
    args: &[&str],
    cwd: &Path,
    env: &[(&str, &str)],
) -> Result<()> {
    let display_cmd = format!("{} {}", program, args.join(" "));
    debug!("running interactive: {display_cmd}");

    which(program)?;

    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(cwd);
    for (k, v) in env {
        cmd.env(k, v);
    }
    // stdin/stdout/stderr are inherited by default in tokio::process::Command.
    let status = cmd.status().await.map_err(ThermiteError::Io)?;
    info!("{display_cmd} exited with {status}");

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        return Err(ThermiteError::CommandFailed {
            cmd: display_cmd,
            code,
            stdout: String::new(),
            stderr: String::new(),
        });
    }
    Ok(())
}

/// Check that `program` is available on `PATH`, returning
/// [`ThermiteError::CommandNotFound`] if it is not.
pub fn which(program: &str) -> Result<String> {
    let output = std::process::Command::new("which")
        .arg(program)
        .output()
        .map_err(|_e| ThermiteError::CommandNotFound(program.to_owned()))?;

    if !output.status.success() {
        return Err(ThermiteError::CommandNotFound(program.to_owned()));
    }

    let output = String::from_utf8(output.stdout)
        .map_err(|_e| ThermiteError::CommandOutputParseError(program.to_owned()))?;

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Finding 11: which() returns a structured error for unknown programs rather
    /// than panicking or returning a confusing OS-level error.
    #[test]
    fn which_returns_error_for_unknown_program() {
        let result = which("this-program-does-not-exist-xyz-thermite");
        assert!(
            result.is_err(),
            "expected CommandNotFound for unknown program"
        );
        assert!(matches!(result, Err(ThermiteError::CommandNotFound(_))));
    }

    /// Finding 4 / Finding 11: run_interactive_command is available as a
    /// separate function with inherited stdio (compile-time check).
    /// A real functional test would require a TTY.
    #[test]
    fn run_interactive_command_is_exported() {
        // Verify the symbol is reachable at compile time.
        // Runtime behaviour (TTY inheritance) is verified via integration tests.
        let _ = run_interactive_command as fn(_, _, _, _) -> _;
    }

    /// The TUI output sink routes `out_line` output into the channel and
    /// `is_sink_active` reflects the state.  The sink is global state, so the
    /// test must always clear it before returning.
    #[test]
    fn output_sink_routes_lines_and_clears() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        set_output_sink(tx);
        assert!(is_sink_active());

        out_line("hello tui");
        assert_eq!(
            rx.try_recv().expect("line should be in the sink"),
            "hello tui"
        );

        clear_output_sink();
        assert!(!is_sink_active());
    }
}
