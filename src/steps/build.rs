use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::shell::run_command;

/// Build a Debian source package with `dpkg-buildpackage -S -I -i -nc -d -sa`.
pub async fn run_dpkg_buildpackage_source(repo_dir: &Path) -> Result<()> {
    run_command(
        "dpkg-buildpackage",
        &["-S", "-I", "-i", "-nc", "-d", "-sa"],
        repo_dir,
        &[],
    )
    .await?;
    Ok(())
}

/// Clean build artifacts from the parent directory and the local `debian/`
/// files.
pub async fn clean_build_artifacts(parent_dir: &Path, repo_dir: &Path) -> Result<()> {
    // Remove generated Debian build output files from the parent directory.
    for suffix in &["debian.tar.xz", "dsc", "buildinfo", "changes", "ppa.upload"] {
        let pattern = format!("*.{suffix}");
        let _ = run_command(
            "bash",
            &["-c", &format!("rm -vf {pattern}")],
            parent_dir,
            &[],
        )
        .await;
    }

    // Remove debian/files and the quilt state directory.
    let debian_files = repo_dir.join("debian/files");
    if debian_files.exists() {
        std::fs::remove_file(&debian_files)?;
    }
    let pc_dir = repo_dir.join(".pc");
    if pc_dir.exists() {
        std::fs::remove_dir_all(&pc_dir)?;
    }
    Ok(())
}

/// The result of an `sbuild` invocation.
#[derive(Debug)]
pub enum SbuildResult {
    Success,
    Failure {
        /// Path to the build log produced by sbuild, or `None` when sbuild
        /// failed before it could create one (e.g. during source packaging).
        log_path: Option<PathBuf>,
        /// Captured sbuild stdout — always present, even on early failure.
        stdout: String,
        /// Captured sbuild stderr — always present, even on early failure.
        stderr: String,
    },
}

/// Run `sbuild -Ad <release>` to build the package in a clean chroot.
///
/// `extra_args` can include flags like `--extra-repository=...` for PPA
/// bootstrapping.
pub async fn run_sbuild(
    repo_dir: &Path,
    release: &str,
    extra_args: &[String],
) -> Result<SbuildResult> {
    let dist_arg = format!("-Ad{release}");
    let mut args = vec![dist_arg.as_str()];
    let extra_refs: Vec<&str> = extra_args.iter().map(|s| s.as_str()).collect();
    args.extend_from_slice(&extra_refs);

    match run_command("sbuild", &args, repo_dir, &[]).await {
        Ok(_) => Ok(SbuildResult::Success),
        Err(crate::error::ThermiteError::CommandFailed { stdout, stderr, .. }) => {
            // Find the most recent .build log sbuild may have produced.
            let parent = repo_dir.parent().unwrap_or(repo_dir);
            let log_path = find_latest_build_log(parent);

            // When sbuild fails before opening its build log (e.g. during
            // `dpkg-source` packaging), there is no `.build` file anywhere.
            // Persist the captured stdout/stderr to a real file so the user
            // always has something to inspect instead of a fictional path.
            let log_path = log_path.or_else(|| {
                let fallback = parent.join("sbuild-failure.log");
                let mut content = String::new();
                if !stdout.is_empty() {
                    content.push_str("=== sbuild stdout ===\n");
                    content.push_str(&stdout);
                    if !content.ends_with('\n') {
                        content.push('\n');
                    }
                }
                if !stderr.is_empty() {
                    content.push_str("=== sbuild stderr ===\n");
                    content.push_str(&stderr);
                    if !content.ends_with('\n') {
                        content.push('\n');
                    }
                }
                if content.is_empty() {
                    content.push_str("(sbuild produced no output)\n");
                }
                // Best-effort write: if it fails, we still return the
                // captured text via the Failure variant so callers can
                // surface it.
                let _ = std::fs::write(&fallback, &content);
                Some(fallback)
            });

            Ok(SbuildResult::Failure {
                log_path,
                stdout,
                stderr,
            })
        }
        Err(e) => Err(e),
    }
}

/// Extract the `stdout ----` sections from an sbuild log, which contain
/// individual test failure output.
pub fn extract_test_failures(sbuild_log: &Path) -> Result<Vec<String>> {
    let content = std::fs::read_to_string(sbuild_log)?;
    let mut sections = Vec::new();
    let mut current: Option<String> = None;

    for line in content.lines() {
        if line.contains("stdout ----") {
            current = Some(line.to_owned() + "\n");
        } else if let Some(ref mut buf) = current {
            buf.push_str(line);
            buf.push('\n');
            // End of section — typically a blank line or dashes.
            if line.trim().is_empty() || line.starts_with("----") {
                sections.push(buf.clone());
                current = None;
            }
        }
    }
    Ok(sections)
}

fn find_latest_build_log(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".build"))
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
}

/// Run `quilt pop -a` to unapply all quilt patches from the working tree.
///
/// This must be called before `clean_build_artifacts` and before
/// `dpkg-buildpackage -S` so that the source package captures patches as
/// quilt series rather than as inline diffs against modified files.
///
/// Errors are silently ignored: the command exits non-zero when no patches
/// are applied, which is the common case.
pub async fn quilt_pop_all(repo_dir: &Path) -> Result<()> {
    // Ignore errors — `quilt pop -a` exits 1 when no patches are applied.
    let _ = run_command("quilt", &["pop", "-a"], repo_dir, &[]).await;
    Ok(())
}

/// Outcome of attempting to remove the self-build test stanza from
/// `debian/tests/control`.
///
/// The removal is driven by [`disable_self_build_test`], which recognises the
/// stanza shapes that upstream Debian has used so far. The caller is expected
/// to handle [`SelfBuildTestOutcome::NeedsManualIntervention`] by prompting
/// the user — see the Phase 7 call site in `commands/backport.rs`.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum SelfBuildTestOutcome {
    /// The self-build stanza was recognised and removed; the file was rewritten.
    Removed,
    /// The self-build marker was not present in the file (already absent).
    AlreadyAbsent,
    /// The marker line was found, but the surrounding stanza does not match
    /// any known shape. The file was left untouched; the caller should ask
    /// the user to remove the stanza manually (or skip the removal).
    NeedsManualIntervention,
}

/// The line that begins the self-build test stanza.
///
/// Used as the anchor for locating the stanza within `debian/tests/control`.
const SELFBUILD_MARKER: &str = "Test-Command: ./debian/rules build RUST_TEST_SELFBUILD=1";

/// The canonical 3-line self-build stanza (older Rust packaging).
const SELFBUILD_STANZA_A: &[&str] = &[
    "Test-Command: ./debian/rules build RUST_TEST_SELFBUILD=1",
    "Depends: @, @builddeps@",
    "Restrictions: rw-build-tree, allow-stderr",
];

/// The 6-line self-build stanza introduced in newer Rust packaging, which
/// adds two explanatory comments and an `Architecture:` restriction.
const SELFBUILD_STANZA_B: &[&str] = &[
    "Test-Command: ./debian/rules build RUST_TEST_SELFBUILD=1",
    "Depends: @, @builddeps@",
    "Restrictions: rw-build-tree, allow-stderr",
    "# Other arches work but are flaky due to memory limitations.",
    "# Additionally, the test doesn't test for anything arch-specific.",
    "Architecture: amd64",
];

/// Remove the autopkgtest self-build test stanza from `debian/tests/control`.
///
/// Two stanza shapes are recognised (after trimming trailing whitespace from
/// each line):
///
/// - **Pattern A** (3 lines): `Test-Command` + `Depends` + `Restrictions`.
/// - **Pattern B** (6 lines): Pattern A plus two comment lines and
///   `Architecture: amd64`.
///
/// Any other shape — a stanza with extra fields, altered comments, or missing
/// lines — is left untouched and reported via
/// [`SelfBuildTestOutcome::NeedsManualIntervention`] so the caller can prompt
/// the user. This keeps the removal future-proof: when upstream Debian next
/// changes the stanza, thermite will refuse to silently mangle the file
/// instead of leaving it half-edited.
///
/// If `debian/tests/control` does not exist or contains no self-build marker,
/// the function succeeds and returns [`SelfBuildTestOutcome::AlreadyAbsent`].
pub fn disable_self_build_test(repo_dir: &Path) -> Result<SelfBuildTestOutcome> {
    let tests_control = repo_dir.join("debian/tests/control");
    if !tests_control.exists() {
        return Ok(SelfBuildTestOutcome::AlreadyAbsent);
    }

    let content = std::fs::read_to_string(&tests_control)?;

    if !content
        .lines()
        .any(|l| l.trim_start().starts_with(SELFBUILD_MARKER))
    {
        return Ok(SelfBuildTestOutcome::AlreadyAbsent);
    }

    let all_lines: Vec<&str> = content.lines().collect();

    // Locate the self-build stanza: its first line is the marker, and it must
    // start a stanza — i.e. it is either at the top of the file or preceded by
    // a blank line. This prevents matching the marker when it appears in the
    // middle of some other stanza.
    let Some(start) = all_lines
        .iter()
        .position(|l| l.trim_start().starts_with(SELFBUILD_MARKER))
    else {
        return Ok(SelfBuildTestOutcome::AlreadyAbsent);
    };
    let starts_new_stanza = start == 0 || all_lines[start - 1].trim().is_empty();
    if !starts_new_stanza {
        return Ok(SelfBuildTestOutcome::NeedsManualIntervention);
    }

    // The stanza runs from `start` until the next blank line or EOF.
    let end = all_lines[start..]
        .iter()
        .position(|l| l.trim().is_empty())
        .map(|offset| start + offset)
        .unwrap_or(all_lines.len());

    let stanza_lines: Vec<&str> = all_lines[start..end].to_vec();

    let normalised: Vec<String> = stanza_lines
        .iter()
        .map(|l| l.trim_start().trim_end().to_owned())
        .collect();

    let matches_a = normalised.as_slice() == SELFBUILD_STANZA_A;
    let matches_b = normalised.as_slice() == SELFBUILD_STANZA_B;

    if !matches_a && !matches_b {
        return Ok(SelfBuildTestOutcome::NeedsManualIntervention);
    }

    // Remove the stanza plus one adjacent blank line so we don't leave a
    // double-blank gap. Prefer the trailing blank line; fall back to the
    // leading one.
    let remove_start = if end < all_lines.len() && all_lines[end].trim().is_empty() {
        start
    } else if start > 0 && all_lines[start - 1].trim().is_empty() {
        start - 1
    } else {
        start
    };
    let remove_end = if end < all_lines.len() && all_lines[end].trim().is_empty() {
        end + 1
    } else {
        end
    };

    let mut out_lines: Vec<&str> =
        Vec::with_capacity(all_lines.len() - (remove_end - remove_start));
    out_lines.extend_from_slice(&all_lines[..remove_start]);
    out_lines.extend_from_slice(&all_lines[remove_end..]);

    // Drop any leading blank lines left at the top of the file after removal.
    while out_lines
        .first()
        .map(|l| l.trim().is_empty())
        .unwrap_or(false)
    {
        out_lines.remove(0);
    }

    let new_content = out_lines.join("\n") + "\n";
    std::fs::write(&tests_control, new_content)?;
    Ok(SelfBuildTestOutcome::Removed)
}

#[cfg(test)]
mod disable_self_build_tests {
    use super::*;
    use std::fs;

    fn write_temp_tests_control_manual(content: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "thermite-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let tests_dir = base.join("debian/tests");
        fs::create_dir_all(&tests_dir).expect("create dirs");
        let control = tests_dir.join("control");
        fs::write(&control, content).expect("write");
        base
    }

    #[test]
    fn removes_self_build_block_pattern_a() {
        let control_content = "\
Test-Command: ./debian/rules build RUST_TEST_SELFBUILD=1
Depends: @, @builddeps@
Restrictions: rw-build-tree, allow-stderr

Test-Command: cargo test
Depends: @
Restrictions: allow-stderr
";
        let repo_dir = write_temp_tests_control_manual(control_content);
        let outcome = disable_self_build_test(&repo_dir).expect("disable_self_build_test");
        assert_eq!(outcome, SelfBuildTestOutcome::Removed);
        let result =
            fs::read_to_string(repo_dir.join("debian/tests/control")).expect("read result");
        assert!(!result.contains("RUST_TEST_SELFBUILD"));
        assert!(result.contains("cargo test"));
        let _ = fs::remove_dir_all(&repo_dir);
    }

    #[test]
    fn removes_self_build_block_pattern_b() {
        let control_content = "\
Test-Command: ./debian/rules build RUST_TEST_SELFBUILD=1
Depends: @, @builddeps@
Restrictions: rw-build-tree, allow-stderr
# Other arches work but are flaky due to memory limitations.
# Additionally, the test doesn't test for anything arch-specific.
Architecture: amd64

Test-Command: cargo test
Depends: @
Restrictions: allow-stderr
";
        let repo_dir = write_temp_tests_control_manual(control_content);
        let outcome = disable_self_build_test(&repo_dir).expect("disable_self_build_test");
        assert_eq!(outcome, SelfBuildTestOutcome::Removed);
        let result =
            fs::read_to_string(repo_dir.join("debian/tests/control")).expect("read result");
        assert!(!result.contains("RUST_TEST_SELFBUILD"));
        assert!(!result.contains("Architecture: amd64"));
        assert!(result.contains("cargo test"));
        let _ = fs::remove_dir_all(&repo_dir);
    }

    #[test]
    fn pattern_b_with_extra_field_needs_manual_intervention() {
        let control_content = "\
Test-Command: ./debian/rules build RUST_TEST_SELFBUILD=1
Depends: @, @builddeps@
Restrictions: rw-build-tree, allow-stderr
# Other arches work but are flaky due to memory limitations.
# Additionally, the test doesn't test for anything arch-specific.
Architecture: amd64 arm64

Test-Command: cargo test
Depends: @
Restrictions: allow-stderr
";
        let repo_dir = write_temp_tests_control_manual(control_content);
        let outcome = disable_self_build_test(&repo_dir).expect("disable_self_build_test");
        assert_eq!(outcome, SelfBuildTestOutcome::NeedsManualIntervention);
        // File must be left untouched.
        let result =
            fs::read_to_string(repo_dir.join("debian/tests/control")).expect("read result");
        assert_eq!(result, control_content);
        let _ = fs::remove_dir_all(&repo_dir);
    }

    #[test]
    fn trailing_whitespace_tolerated() {
        let control_content = "\
Test-Command: ./debian/rules build RUST_TEST_SELFBUILD=1   
Depends: @, @builddeps@   
Restrictions: rw-build-tree, allow-stderr   

Test-Command: cargo test
Depends: @
Restrictions: allow-stderr
";
        let repo_dir = write_temp_tests_control_manual(control_content);
        let outcome = disable_self_build_test(&repo_dir).expect("disable_self_build_test");
        assert_eq!(outcome, SelfBuildTestOutcome::Removed);
        let result =
            fs::read_to_string(repo_dir.join("debian/tests/control")).expect("read result");
        assert!(!result.contains("RUST_TEST_SELFBUILD"));
        assert!(result.contains("cargo test"));
        let _ = fs::remove_dir_all(&repo_dir);
    }

    #[test]
    fn marker_alone_without_depends_needs_manual_intervention() {
        let control_content = "\
Test-Command: ./debian/rules build RUST_TEST_SELFBUILD=1

Test-Command: cargo test
Depends: @
Restrictions: allow-stderr
";
        let repo_dir = write_temp_tests_control_manual(control_content);
        let outcome = disable_self_build_test(&repo_dir).expect("disable_self_build_test");
        assert_eq!(outcome, SelfBuildTestOutcome::NeedsManualIntervention);
        let result =
            fs::read_to_string(repo_dir.join("debian/tests/control")).expect("read result");
        assert_eq!(result, control_content);
        let _ = fs::remove_dir_all(&repo_dir);
    }

    #[test]
    fn no_op_when_block_absent() {
        let control_content = "\
Test-Command: cargo test
Depends: @
Restrictions: allow-stderr
";
        let repo_dir = write_temp_tests_control_manual(control_content);
        let outcome = disable_self_build_test(&repo_dir).expect("no_op");
        assert_eq!(outcome, SelfBuildTestOutcome::AlreadyAbsent);
        let result =
            fs::read_to_string(repo_dir.join("debian/tests/control")).expect("read result");
        assert_eq!(result.trim(), control_content.trim());
        let _ = fs::remove_dir_all(&repo_dir);
    }

    #[test]
    fn no_op_when_control_file_missing() {
        let base = std::env::temp_dir().join(format!(
            "thermite-test-missing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let outcome = disable_self_build_test(&base).expect("missing file");
        assert_eq!(outcome, SelfBuildTestOutcome::AlreadyAbsent);
    }

    #[test]
    fn removes_leading_blanks_after_top_of_file_removal() {
        let control_content = "\
Test-Command: ./debian/rules build RUST_TEST_SELFBUILD=1
Depends: @, @builddeps@
Restrictions: rw-build-tree, allow-stderr

Test-Command: cargo test
Depends: @
Restrictions: allow-stderr
";
        let repo_dir = write_temp_tests_control_manual(control_content);
        let outcome = disable_self_build_test(&repo_dir).expect("disable_self_build_test");
        assert_eq!(outcome, SelfBuildTestOutcome::Removed);
        let result =
            fs::read_to_string(repo_dir.join("debian/tests/control")).expect("read result");
        assert!(
            !result.starts_with('\n'),
            "no leading blank line after removal: {result:?}"
        );
        let _ = fs::remove_dir_all(&repo_dir);
    }
}
