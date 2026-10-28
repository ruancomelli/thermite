use std::io::{self, Write};

use dialoguer::{Input, Select, theme::ColorfulTheme};

use crate::shell;

/// Returns `true` when stdout is an interactive terminal.
///
/// When `false` (piped, redirected, or a dumb terminal), all ANSI escape codes
/// are suppressed so that log files and screen-reader output stay clean.
fn is_tty() -> bool {
    use std::io::IsTerminal;
    io::stdout().is_terminal()
}

/// Returns `true` when running under the TUI (output sink active).
///
/// In TUI mode the alternate screen is owned by the ratatui event loop, so:
/// - ANSI escape codes are suppressed (they would render as garbage in the
///   log pane),
/// - interactive prompts must not touch the terminal.
fn is_tui() -> bool {
    shell::is_sink_active()
}

/// Print a formatted phase header banner to stdout.
///
/// On a TTY the title is rendered as a bold reverse-video (inverted) banner,
/// which is visually prominent without emitting any decorative characters that
/// a screen reader would enumerate.  On non-TTY output the plain text title is
/// printed instead.  Under the TUI the plain text goes into the log pane.
pub fn print_phase_header(phase: u32, title: &str) {
    let label = format!("  THERMITE \u{2014} Phase {phase}: {title}");
    if is_tui() {
        shell::out_line("");
        shell::out_line(&label);
    } else if is_tty() {
        // Pad to a fixed display width and wrap in bold + reverse-video.
        // \x1b[1m = bold, \x1b[7m = reverse video, \x1b[0m = reset.
        let padded = format!("{label:<70}");
        shell::out_line(&format!("\n\x1b[1m\x1b[7m{padded}\x1b[0m"));
    } else {
        shell::out_line(&format!("\n{label}"));
    }
}

/// Print a tool-check list with a `✔`/`✗` indicator for each entry.
///
/// `checks` is a slice of `(tool_name, found)` pairs. Found tools get a `✔`
/// prefix; missing tools get a `✗` prefix. The section is formatted to match
/// the visual style of [`print_info_box`] so it reads as a natural peer of
/// "About this phase" without needing box-drawing characters.
///
/// Call this after [`crate::commands::backport::print_phase_explanation`] and before the parameters box
/// so the user sees every check result before being asked to confirm.
pub fn print_tool_checks(checks: &[(&str, bool)]) {
    if is_tty() && !is_tui() {
        shell::out_line("\x1b[1m  Required tools\x1b[0m");
    } else {
        shell::out_line("  Required tools");
    }
    shell::out_line("");
    for (name, found) in checks {
        if *found {
            shell::out_line(&format!("    \u{2714} {name}"));
        } else {
            shell::out_line(&format!("    \u{2717} {name}"));
        }
    }
    shell::out_line("");
}

/// Print a formatted info box with a title and body lines.
///
/// On a TTY the title is rendered in bold, and the body lines are indented
/// beneath it with a blank separator line — visually clear, accessible to
/// screen readers, and free of decorative box-drawing characters.
pub fn print_info_box(title: &str, lines: &[&str]) {
    let width = 66usize;
    if is_tty() && !is_tui() {
        // \x1b[1m = bold, \x1b[0m = reset.
        shell::out_line(&format!("\x1b[1m  {title}\x1b[0m"));
    } else {
        shell::out_line(&format!("  {title}"));
    }
    shell::out_line("");
    for line in lines {
        if line.is_empty() {
            shell::out_line("");
        } else {
            for chunk in wrap_line(line, width - 4) {
                shell::out_line(&format!("    {chunk}"));
            }
        }
    }
    shell::out_line("");
}

/// Wrap a single line to at most `max_width` *characters* per chunk.
///
/// Leading whitespace on the input line is detected and reused as a hanging
/// indent on every continuation chunk, so indented content like
/// `"  If foo: long description..."` wraps with its indent preserved rather
/// than losing it on the second line.
///
/// Split points are only searched in the *content* (the part after the leading
/// indent) so that indent spaces are never mistaken for word boundaries.  When
/// no space exists within `max_width` characters of the content (e.g. a long
/// URL), the line is extended to the next natural word boundary rather than
/// hard-breaking mid-word.  If no word boundary exists at all the whole line
/// is emitted as-is.
fn wrap_line(line: &str, max_width: usize) -> Vec<String> {
    if line.chars().count() <= max_width {
        return vec![line.to_owned()];
    }
    // Detect the leading whitespace to reuse as a hanging indent on continuations.
    let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
    let mut chunks = Vec::new();
    let mut current = line.to_owned();
    loop {
        if current.chars().count() <= max_width {
            chunks.push(current);
            break;
        }
        // Find the byte index of char `max_width` (first char that would exceed the limit).
        let byte_limit = current
            .char_indices()
            .nth(max_width)
            .map(|(i, _)| i)
            .unwrap_or(current.len());
        // Only search for a split point within the *content* (after the indent)
        // to avoid splitting on an indent space and producing empty chunks.
        let content_start = indent.len().min(byte_limit);
        let split_at = current[content_start..byte_limit]
            .rfind(' ')
            .map(|i| content_start + i + 1)
            .or_else(|| {
                // No space within max_width after the indent (e.g. a long URL).
                // Extend to the next natural word boundary to avoid mid-word breaks.
                current[content_start..]
                    .find(' ')
                    .map(|i| content_start + i + 1)
            });
        match split_at {
            Some(split_at) => {
                chunks.push(current[..split_at].trim_end().to_owned());
                let rest = &current[split_at..];
                if rest.is_empty() {
                    break;
                }
                // Prepend the original indent to continuation lines so indented content
                // wraps with the same leading whitespace rather than losing it.
                current = format!("{indent}{rest}");
            }
            None => {
                // No word boundary anywhere in the content — emit the whole line as-is.
                chunks.push(current);
                break;
            }
        }
    }
    chunks
}

/// Prompt the user to press Enter to continue. Returns when Enter is pressed.
///
/// This is intentionally a plain readline prompt rather than a dialoguer
/// primitive — dialoguer has no "press Enter to continue" type, and the simple
/// implementation is correct and sufficient here.
///
/// Under the TUI there is no stdin to wait on, so the message is logged and
/// the function returns immediately.
pub fn prompt_continue(message: &str) {
    if is_tui() {
        shell::out_line(&format!("  {message}"));
        return;
    }
    print!("\n  {message} [Press Enter to continue] ");
    io::stdout().flush().ok();
    let mut buf = String::new();
    io::stdin().read_line(&mut buf).ok();
}

/// Prompt the user with a yes/no question.
///
/// Returns `true` if the user answers `y` or `yes` (case-insensitive). Any
/// other input (including Enter alone) returns `false`, making the default
/// safe-conservative "no". The `[y/N]` suffix makes the default visible.
///
/// Under the TUI the question is logged and the conservative default
/// (`false`) is returned.
pub fn prompt_yes_no(question: &str) -> bool {
    if is_tui() {
        shell::out_line(&format!(
            "  {question} [y/N] (auto-answer: no under the TUI)"
        ));
        return false;
    }
    print!("\n  {question} [y/N] ");
    io::stdout().flush().ok();
    let mut buf = String::new();
    io::stdin().read_line(&mut buf).ok();
    matches!(buf.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Prompt the user for a line of free-form text input. Returns the trimmed
/// string. Falls back to an empty string if the terminal is not interactive.
///
/// Under the TUI the prompt is logged and an empty string is returned; TUI
/// interactions use their own ratatui modal input instead.
pub fn prompt_input(prompt: &str) -> String {
    if is_tui() {
        shell::out_line(&format!("  {prompt} (auto-answer: empty under the TUI)"));
        return String::new();
    }
    println!();
    Input::<String>::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .allow_empty(true)
        .interact_text()
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// Display a numbered, arrow-key-navigable menu and return the index of the
/// selected option.
///
/// `default` is the index pre-highlighted when the menu opens. Falls back to
/// `default` if the terminal is not interactive.
///
/// Under the TUI the menu cannot be rendered inside the alternate screen
/// (raw mode is owned by the ratatui event loop), so the menu is logged and
/// the default index is returned — the same fallback as non-TTY mode.
pub fn prompt_select(prompt: &str, options: &[&str], default: usize) -> usize {
    if is_tui() {
        shell::out_line(&format!(
            "  {prompt} (auto-selecting the default under the TUI)"
        ));
        for (i, option) in options.iter().enumerate() {
            shell::out_line(&format!("    {i}. {option}"));
        }
        return default;
    }
    println!();
    Select::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .items(options)
        .default(default)
        .interact()
        .unwrap_or(default)
}

/// Gate for a dangerous, hard-to-revert ("sink") operation.
///
/// **Dry-run mode** (`dry_run = true`): prints `[dry-run] SKIP: <description>`
/// with each detail line indented beneath it, then returns `false` — no prompt,
/// no execution.
///
/// **Normal mode**: shows a `WARNING` info box displaying `target_lines`
/// followed by a note that the operation cannot be automatically reversed,
/// then delegates to [`prompt_yes_no`]. Returns the user's answer.
///
/// # Usage
///
/// Call this immediately before every `git push`, `ppa create`, or `dput`.
/// If it returns `false`, skip the operation entirely.
///
/// ```ignore
/// if confirm_sink(dry_run, "Push branch to remote", &["  Branch: main", "  Remote: origin"]) {
///     // execute the push
/// }
/// ```
pub fn confirm_sink(dry_run: bool, description: &str, target_lines: &[&str]) -> bool {
    if dry_run {
        shell::out_line(&format!("\n  [dry-run] SKIP: {description}"));
        for line in target_lines {
            if !line.trim().is_empty() {
                shell::out_line(&format!("            {line}"));
            }
        }
        return false;
    }
    let title = format!("WARNING \u{2014} {description}");
    let mut lines: Vec<&str> = target_lines.to_vec();
    lines.extend_from_slice(&["", "This operation cannot be automatically reversed."]);
    print_info_box(&title, &lines);
    prompt_yes_no("Proceed with this irreversible operation?")
}

/// Print a countdown from `secs` to 1, rewriting the same terminal line each
/// second, then erase the line when done.
///
/// Call this immediately after printing a "Retrying in X seconds" message.
pub async fn countdown_secs(secs: u64) {
    for remaining in (1..=secs).rev() {
        if is_tui() {
            // The `\r` line-rewriting below would only corrupt the log pane;
            // emit one plain line per second instead.
            shell::out_line(&format!("  {remaining:3}s remaining..."));
        } else {
            print!("\r  {remaining:3}s remaining...   ");
            io::stdout().flush().ok();
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    }
    if !is_tui() {
        // Erase the countdown line so subsequent output starts cleanly.
        print!("\r{:30}\r", "");
        io::stdout().flush().ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In dry-run mode, `confirm_sink` must return `false` without touching
    /// stdin (a test that blocks on stdin would hang the suite).
    #[test]
    fn confirm_sink_dry_run_returns_false_without_prompting() {
        // Pass non-empty target lines to exercise the printing branch.
        let result = confirm_sink(
            true,
            "Upload to shared PPA",
            &["  PPA: rust-toolchain/staging", "  Package: foo.changes"],
        );
        assert!(!result, "confirm_sink in dry-run mode must return false");
    }

    /// In dry-run mode, empty / whitespace-only target lines are silently
    /// skipped (no blank indented lines in the output).
    #[test]
    fn confirm_sink_dry_run_skips_blank_lines() {
        // Should not panic; blank lines are filtered out.
        let result = confirm_sink(true, "Create PPA", &["", "  PPA: my-ppa", "  "]);
        assert!(!result);
    }

    /// `wrap_line` must respect Unicode character boundaries, not byte lengths.
    #[test]
    fn wrap_line_unicode_width() {
        // "─" is 3 bytes but 1 character; a line of 62 such chars should not
        // be split (62 chars == max_width).
        let line: String = "─".repeat(62);
        let chunks = wrap_line(&line, 62);
        assert_eq!(chunks.len(), 1, "62-char Unicode line should not be split");
    }

    /// `wrap_line` must preserve leading whitespace as a hanging indent on
    /// every continuation chunk so that indented list items re-indent correctly
    /// after wrapping instead of losing their leading spaces.
    #[test]
    fn wrap_line_preserves_leading_indent() {
        let line =
            "  If foo is not available: use the system package instead of the vendored copy.";
        // max_width = 40 forces the line to wrap at least once.
        let chunks = wrap_line(line, 40);
        assert!(chunks.len() > 1, "line should wrap into multiple chunks");
        // The first chunk must start with the original 2-space indent.
        assert!(
            chunks[0].starts_with("  "),
            "first chunk must keep leading indent, got: {:?}",
            chunks[0]
        );
        // Every continuation chunk must also carry the 2-space hanging indent.
        for chunk in &chunks[1..] {
            assert!(
                chunk.starts_with("  "),
                "continuation chunk must preserve leading 2-space indent, got: {chunk:?}"
            );
        }
    }

    /// `wrap_line` with no leading whitespace must not add spurious indentation
    /// to continuation chunks.
    #[test]
    fn wrap_line_no_indent_no_hanging() {
        let line = "This is a long sentence with no leading whitespace that should wrap cleanly.";
        let chunks = wrap_line(line, 40);
        assert!(chunks.len() > 1, "line should wrap");
        for chunk in &chunks[1..] {
            assert!(
                !chunk.starts_with(' '),
                "continuation chunk of an unindented line must not gain indent, got: {chunk:?}"
            );
        }
    }

    /// An indented URL that has no spaces after the leading indent must not
    /// cause an infinite loop and must be emitted as a single chunk (no
    /// mid-URL hard-break).
    #[test]
    fn wrap_line_indented_url_no_infinite_loop() {
        let url = "  https://launchpad.net/~rust-toolchain/+archive/ubuntu/staging/";
        // The URL is 65 chars, max_width 62 — would previously infinite-loop.
        let chunks = wrap_line(url, 62);
        assert_eq!(
            chunks,
            vec![url.to_owned()],
            "indented URL with no spaces should be emitted as-is"
        );
    }
}
