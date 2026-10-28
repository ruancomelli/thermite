//! The `thermite tui` command.
//!
//! A terminal user interface that wraps the packaging step commands: build,
//! source build, lintian, version bumps, tarball operations, and git fetch.
//! External command output is streamed into an on-screen log pane via the
//! output sink installed in [`crate::shell`], so the alternate screen is
//! never corrupted by stray writes.
//!
//! V1 limitations:
//! - one external command at a time (no parallel runs, no abort);
//! - no interactive commands (`run_interactive_command` users) — the TUI only
//!   invokes commands whose I/O flows through the output sink.

mod app;
mod render;

use std::io;
use std::path::Path;

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::error::Result;
use crate::shell;

use app::{App, AppAction};

/// Outcome of a finished TUI command, sent over the completion channel.
type Completion = (String, Result<String>);

/// Run the TUI over the package rooted at `repo_dir`.
pub async fn run(repo_dir: &Path) -> Result<()> {
    // 1. Install the output sink: every line printed by `shell::run_command`,
    //    `ui::print_*`, and tracing events flows into this channel and is
    //    rendered in the TUI log pane.
    let (log_tx, log_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    shell::set_output_sink(log_tx.clone());
    install_tracing_sink(log_tx.clone());

    // 2. Terminal setup with a panic hook that always restores the terminal.
    install_panic_restore_hook();
    let mut terminal = match setup_terminal() {
        Ok(t) => t,
        Err(e) => {
            shell::clear_output_sink();
            return Err(e);
        }
    };

    let mut app = App::new(repo_dir);
    app.push_log("thermite tui — press a command key from the sidebar; q quits");

    let (done_tx, done_rx) = tokio::sync::mpsc::unbounded_channel::<Completion>();

    let result = event_loop(&mut terminal, &mut app, log_rx, done_tx, done_rx).await;

    restore_terminal(&mut terminal);
    shell::clear_output_sink();
    let _ = std::panic::take_hook(); // uninstall the restore hook
    result
}

/// Terminal setup: raw mode + alternate screen, with a panic hook that
/// restores the terminal so a panic never leaves a broken shell.
fn install_panic_restore_hook() {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(io::stdout(), crossterm::terminal::LeaveAlternateScreen);
        original_hook(info);
    }));
}

/// Enter the alternate screen with raw mode enabled.
fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.hide_cursor()?;
    Ok(terminal)
}

/// Leave the alternate screen and restore cooked mode (best effort).
fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) {
    let _ = terminal.show_cursor();
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = crossterm::execute!(io::stdout(), crossterm::terminal::LeaveAlternateScreen);
}

/// Install a tracing subscriber whose output goes into the TUI log sink.
fn install_tracing_sink(tx: UnboundedSender<String>) {
    use tracing_subscriber::fmt::MakeWriter;

    struct SinkMakeWriter {
        tx: UnboundedSender<String>,
    }

    impl<'a> MakeWriter<'a> for SinkMakeWriter {
        type Writer = SinkWriter;

        fn make_writer(&'a self) -> Self::Writer {
            SinkWriter {
                tx: self.tx.clone(),
            }
        }
    }

    struct SinkWriter {
        tx: UnboundedSender<String>,
    }

    impl io::Write for SinkWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let text = String::from_utf8_lossy(buf);
            for line in text.trim_end_matches('\n').split('\n') {
                let _ = self.tx.send(line.to_owned());
            }
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(SinkMakeWriter { tx })
        .init();
}

/// The main select loop: render, then wait for key events, log lines, or
/// command completions.
async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    mut log_rx: UnboundedReceiver<String>,
    done_tx: UnboundedSender<Completion>,
    mut done_rx: UnboundedReceiver<Completion>,
) -> Result<()> {
    // Spawn a blocking thread that forwards terminal input events into a
    // channel so the select loop never blocks on stdin.
    let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    std::thread::spawn(move || {
        while let Ok(event) = crossterm::event::read() {
            if ev_tx.send(event).is_err() {
                break;
            }
        }
    });

    loop {
        terminal
            .draw(|frame| render::draw(frame, app))
            .map_err(crate::error::ThermiteError::Io)?;

        if app.should_quit {
            return Ok(());
        }

        tokio::select! {
            // Terminal input events (forwarded by the reader thread).
            event = ev_rx.recv() => {
                let Some(event) = event else {
                    // Reader thread died — treat as quit.
                    return Ok(());
                };
                if let Event::Key(key) = event {
                    // Ignore Release events; handle Press and Repeat.
                    if key.kind == KeyEventKind::Release {
                        continue;
                    }
                    // Ctrl+C always quits.
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('c')
                    {
                        if app.busy.is_some() {
                            app.push_log(
                                "  ctrl-c while a command is running is ignored in v1 (no abort support); wait for the command to finish",
                            );
                            continue;
                        }
                        app.should_quit = true;
                        return Ok(());
                    }
                    match app.handle_key(key) {
                        AppAction::None => {}
                        AppAction::Quit => {
                            app.should_quit = true;
                            return Ok(());
                        }
                        AppAction::Run { description, command } => {
                            spawn_command(app, done_tx.clone(), description, command);
                        }
                    }
                }
            }
            // Streaming output lines from run_command / tracing.
            Some(line) = log_rx.recv() => {
                app.push_log(&line);
            }
            // Command completion notifications.
            Some((description, outcome)) = done_rx.recv() => {
                app.on_command_done(&description, outcome);
            }
        }
    }
}

/// Spawn the resolved command as a tokio task; its output flows into the log
/// sink automatically because `shell::set_output_sink` is active.
fn spawn_command(
    app: &mut App,
    done_tx: UnboundedSender<Completion>,
    description: String,
    command: app::ResolvedCommand,
) {
    app.busy = Some(description.clone());
    app.push_log(&format!("== started: {description}"));
    let repo_dir = app.repo_dir.clone();
    tokio::spawn(async move {
        let outcome = execute(&command, &repo_dir).await;
        let _ = done_tx.send((description, outcome));
    });
}

/// Execute a resolved command against the repo, returning a human summary.
async fn execute(command: &app::ResolvedCommand, repo_dir: &Path) -> Result<String> {
    use crate::commands::tarball;
    use crate::steps::{build, changelog, git, lintian};

    match command {
        app::ResolvedCommand::Build { release } => {
            let result = build::run_sbuild(repo_dir, release, &[]).await?;
            match result {
                build::SbuildResult::Success => Ok("sbuild completed successfully".to_owned()),
                build::SbuildResult::Failure { log_path, .. } => {
                    let log_note = log_path.as_deref().map_or_else(
                        || "<no log>".to_owned(),
                        |p| p.to_string_lossy().to_string(),
                    );
                    Err(crate::error::ThermiteError::CommandFailed {
                        cmd: format!("sbuild ({release})"),
                        code: 1,
                        stdout: String::new(),
                        stderr: format!("build failed — inspect {log_note}"),
                    })
                }
            }
        }
        app::ResolvedCommand::SourceBuild => {
            build::run_dpkg_buildpackage_source(repo_dir).await?;
            Ok("dpkg-buildpackage -S completed".to_owned())
        }
        app::ResolvedCommand::Lintian => {
            let log_path = repo_dir
                .parent()
                .unwrap_or(repo_dir)
                .join("lintian-tui.log");
            let output =
                lintian::run_lintian(repo_dir, &["-i", "-I", "-E", "--pedantic"], &log_path)
                    .await?;
            Ok(format!(
                "lintian finished: {} errors, {} warnings (log: {})",
                output.errors.len(),
                output.warnings.len(),
                log_path.display()
            ))
        }
        app::ResolvedCommand::Bump { operation } => {
            let changelog_path = repo_dir.join("debian/changelog");
            let new_version = changelog::apply_version_bump(&changelog_path, operation)?;
            Ok(format!("version bumped to {new_version}"))
        }
        app::ResolvedCommand::SetVersion { version } => {
            let changelog_path = repo_dir.join("debian/changelog");
            changelog::set_changelog_version(&changelog_path, version)?;
            Ok(format!("version set to {version}"))
        }
        app::ResolvedCommand::Tarball { params, target } => {
            tarball::run(params, repo_dir, *target).await?;
            Ok("tarball operation completed".to_owned())
        }
        app::ResolvedCommand::GitFetch => {
            git::fetch_all(repo_dir).await?;
            Ok("git fetch --all completed".to_owned())
        }
    }
}
