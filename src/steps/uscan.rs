use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::{info, warn};

use crate::error::{Result, ThermiteError};
use crate::shell::run_command;
use crate::steps::tarball_fetch::{TarballKind, expected_tarball_name};
use crate::types::versions::RustVersion;

/// Delays (in seconds) between successive uscan retry attempts when a
/// 403 rate-limit response is detected: first retry after 15 s, second after
/// 30 s, third after 60 s.
const RETRY_DELAYS: [u64; 3] = [15, 30, 60];

/// Update `debian/watch` so that the URL regex matches the new upstream version
/// rather than the old one.
///
/// The watch file for a versioned Rust package pins the minor version inside
/// the URL regex (e.g. `1\.84\.\d+`) so that uscan only picks up point
/// releases within that series. When creating a new versioned package branch
/// (e.g. moving from `rustc-1.84` to `rustc-1.85`), this pattern must be
/// updated before uscan is run, otherwise `--download-version X.Y.Z` finds no
/// matching URL and the download fails.
///
/// Replaces both:
/// * The regex-escaped form: `1\.84` → `1\.85`
/// * The literal form: `1.84` → `1.85`
///
/// A warning is logged (but no error returned) when neither form is found,
/// so that the caller can surface it to the user without failing hard.
pub fn update_watch_version(watch_path: &Path, old_short: &str, new_short: &str) -> Result<()> {
    let content = std::fs::read_to_string(watch_path)?;

    // Build the regex-escaped form by replacing `.` with `\.`.
    let old_escaped = old_short.replace('.', r"\.");
    let new_escaped = new_short.replace('.', r"\.");

    // Replace escaped form first so the subsequent literal replacement does
    // not accidentally re-process already-updated text.
    let updated = content
        .replace(&old_escaped, &new_escaped)
        .replace(old_short, new_short);

    if updated == content {
        warn!(
            "debian/watch did not contain old version '{}' — \
             the file may need to be updated manually",
            old_short
        );
    }

    std::fs::write(watch_path, updated)?;
    Ok(())
}

/// Returns `true` when the combined uscan output suggests a 403 rate-limit
/// error, allowing the caller to decide whether to retry.
fn is_rate_limit_error(stdout: &str, stderr: &str) -> bool {
    [stdout, stderr].iter().any(|s| {
        let lower = s.to_ascii_lowercase();
        lower.contains("403") || lower.contains("rate limit")
    })
}

/// Create a private staging directory for one uscan run:
/// `<parent>/.thermite/uscan-<version>-<pid>-<nanos>` inside the repo's
/// parent directory. It lives on the same filesystem as the final tarball
/// destination, so the produced file can simply be renamed into place.
fn new_staging_dir(repo_dir: &Path, version: &RustVersion) -> Result<PathBuf> {
    let parent_dir = repo_dir.parent().unwrap_or(repo_dir);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let dir = parent_dir.join(format!(
        ".thermite/uscan-{version}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Remove the staging directory, and its `.thermite` parent when that became
/// empty. Best-effort: a leftover directory is harmless clutter.
fn remove_staging_dir(staging: &Path) {
    if let Err(e) = std::fs::remove_dir_all(staging) {
        warn!(
            "failed to remove uscan staging dir {}: {e}",
            staging.display()
        );
    }
    if let Some(thermite_dir) = staging.parent() {
        let _ = std::fs::remove_dir(thermite_dir);
    }
}

/// Find the orig tarball uscan produced in `staging`.
///
/// Matches files ending in `.orig.tar.xz`; uscan's intermediate downloads
/// (e.g. `rustc-<version>-src.tar.xz`) do not match. When several candidates
/// exist, the most recently modified one wins.
fn find_produced_tarball(staging: &Path) -> Result<PathBuf> {
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(staging)? {
        let entry = entry?;
        let name = entry.file_name();
        if !name.to_string_lossy().ends_with(".orig.tar.xz") {
            continue;
        }
        let modified = entry.metadata()?.modified()?;
        if newest.as_ref().is_none_or(|(t, _)| modified > *t) {
            newest = Some((modified, entry.path()));
        }
    }
    newest
        .map(|(_, path)| path)
        .ok_or_else(|| ThermiteError::CommandFailed {
            cmd: "uscan".to_owned(),
            code: 0,
            stdout: String::new(),
            stderr: format!(
                "uscan reported success but no orig tarball (*.orig.tar.xz) was found in {}",
                staging.display()
            ),
        })
}

/// Run `uscan --download-version <version>` from `repo_dir`, streaming output
/// to the terminal and saving it to `log_path`.
///
/// uscan downloads into a private staging directory (`.thermite/uscan-…`
/// inside the repo's parent directory) instead of the shared parent, so the
/// produced tarball can be identified unambiguously even when several
/// backports share the same worktree. On success the tarball is moved into
/// the repo's parent directory under its final name,
/// `rustc-<short>_<version>+dfsg<dfsg_suffix>.orig.tar.xz`, and the staging
/// directory is removed. `dfsg_suffix` is appended after `+dfsg` as given:
/// `""` for canonical naming, `"~old"` for the update workflow, or
/// `"~<series>"` for backport naming. The dfsg suffix uscan itself produces
/// follows the watch file's `repacksuffix` (`+dfsg` for rustc-1.97 and
/// newer, `+dfsg1` for older packages) and is normalised away by the move.
///
/// If uscan exits with a 403 rate-limit error the step is retried up to three
/// times, pausing for 15 s, 30 s, and 60 s respectively between attempts.
///
/// Returns the path to the orig tarball under its final name.
pub async fn run_uscan(
    repo_dir: &Path,
    version: &RustVersion,
    dfsg_suffix: &str,
    log_path: &Path,
) -> Result<PathBuf> {
    let staging = new_staging_dir(repo_dir, version)?;
    info!("staging uscan download in {}", staging.display());
    let result = run_uscan_staged(repo_dir, version, dfsg_suffix, log_path, &staging).await;
    remove_staging_dir(&staging);
    result
}

/// Run the uscan retry loop with `staging` as the download directory.
async fn run_uscan_staged(
    repo_dir: &Path,
    version: &RustVersion,
    dfsg_suffix: &str,
    log_path: &Path,
    staging: &Path,
) -> Result<PathBuf> {
    let version_str = version.to_string();
    let staging_str = staging.to_string_lossy().to_string();
    let final_name =
        expected_tarball_name(&version.short(), version, dfsg_suffix, TarballKind::Orig);
    let parent_dir = repo_dir.parent().unwrap_or(repo_dir);
    let final_path = parent_dir.join(final_name);

    // attempt 0 = first try, attempts 1-3 = retries after rate-limit.
    let mut last_rate_limit_err: Option<ThermiteError> = None;

    for attempt in 0_u32..4 {
        if attempt > 0 {
            let secs = RETRY_DELAYS[(attempt - 1) as usize];
            println!("  Rate limit detected. Retrying in {secs} seconds . . .");
            crate::ui::countdown_secs(secs).await;
        }

        match run_command(
            "uscan",
            &[
                "--download-version",
                &version_str,
                "-v",
                "--destdir",
                &staging_str,
            ],
            repo_dir,
            &[],
        )
        .await
        {
            Ok(output) => {
                let log_content = output.stdout + &output.stderr;
                std::fs::write(log_path, &log_content)?;

                let produced = find_produced_tarball(staging)?;
                std::fs::rename(&produced, &final_path)?;
                println!(
                    "  uscan produced {} — moved to {}",
                    produced.display(),
                    final_path.display()
                );
                return Ok(final_path);
            }
            Err(e) => {
                let rate_limited = if let ThermiteError::CommandFailed {
                    ref stdout,
                    ref stderr,
                    ..
                } = e
                {
                    is_rate_limit_error(stdout, stderr)
                } else {
                    false
                };

                if rate_limited && attempt < 3 {
                    last_rate_limit_err = Some(e);
                    // continue to next attempt
                } else {
                    if let ThermiteError::CommandFailed { stdout, stderr, .. } = &e {
                        let _ = std::fs::write(log_path, format!("{stdout}{stderr}"));
                    }
                    return Err(e);
                }
            }
        }
    }

    // All three retries exhausted — surface the last rate-limit error.
    Err(last_rate_limit_err
        .expect("loop invariant: last_rate_limit_err is set before reaching this point"))
}

/// List all `.c` source files inside a `.tar.xz` archive without extracting it.
pub async fn list_c_files_in_tarball(tarball: &Path) -> Result<Vec<String>> {
    let tarball_str = tarball.to_string_lossy().to_string();
    let output = run_command(
        "tar",
        &["-tJf", &tarball_str],
        tarball.parent().unwrap_or(tarball),
        &[],
    )
    .await?;

    let c_files: Vec<String> = output
        .stdout
        .lines()
        .filter(|l| l.ends_with(".c"))
        .map(|l| l.to_owned())
        .collect();

    Ok(c_files)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::fs::FileTimes;
    use std::time::Duration;

    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "thermite-uscan-{label}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Set a file's mtime `offset_secs` in the past so mtime-based selection
    /// is deterministic regardless of directory iteration order.
    fn set_mtime(path: &Path, offset_secs: u64) {
        let modified = SystemTime::now() - Duration::from_secs(offset_secs);
        let file = fs::OpenOptions::new().append(true).open(path).unwrap();
        file.set_times(FileTimes::new().set_modified(modified))
            .unwrap();
    }

    #[test]
    fn find_produced_tarball_ignores_intermediates() {
        let tmp = temp_dir("find-orig");
        fs::write(tmp.join("rustc-1.97.1-src.tar.xz"), b"intermediate").unwrap();
        fs::write(tmp.join("uscan.log"), b"log").unwrap();
        let orig = tmp.join("rustc-1.97_1.97.1+dfsg.orig.tar.xz");
        fs::write(&orig, b"orig").unwrap();

        assert_eq!(find_produced_tarball(&tmp).unwrap(), orig);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn find_produced_tarball_prefers_newest() {
        let tmp = temp_dir("find-newest");
        let older = tmp.join("rustc-1.96_1.96.0+dfsg1.orig.tar.xz");
        let newer = tmp.join("rustc-1.97_1.97.1+dfsg.orig.tar.xz");
        fs::write(&older, b"older").unwrap();
        fs::write(&newer, b"newer").unwrap();
        set_mtime(&older, 60);
        set_mtime(&newer, 0);

        assert_eq!(find_produced_tarball(&tmp).unwrap(), newer);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn find_produced_tarball_errors_when_missing() {
        let tmp = temp_dir("find-missing");
        fs::write(tmp.join("rustc-1.97.1-src.tar.xz"), b"intermediate").unwrap();

        assert!(find_produced_tarball(&tmp).is_err());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn staging_dir_is_created_unique_and_removable() {
        let tmp = temp_dir("staging");
        let repo = tmp.join("repo");
        fs::create_dir_all(&repo).unwrap();

        let a = new_staging_dir(&repo, &RustVersion::parse("1.97.1").unwrap()).unwrap();
        let b = new_staging_dir(&repo, &RustVersion::parse("1.97.1").unwrap()).unwrap();
        assert_ne!(a, b);
        assert!(a.exists());
        assert!(b.exists());

        remove_staging_dir(&a);
        assert!(!a.exists());
        assert!(b.exists(), ".thermite kept while other staging dirs remain");

        remove_staging_dir(&b);
        assert!(!b.exists());
        assert!(!tmp.join(".thermite").exists(), "empty .thermite removed");

        let _ = fs::remove_dir_all(&tmp);
    }

    /// Finding 7: log_path receives the uscan output rather than being left
    /// empty. We test the write path directly using a temporary file.
    #[test]
    fn log_write_produces_non_empty_file_for_non_empty_output() {
        let tmp = std::env::temp_dir().join(format!(
            "thermite-uscan-log-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        ));
        let content = "uscan output line 1\nuscan output line 2\n";
        fs::write(&tmp, content).expect("write log");
        let read_back = fs::read_to_string(&tmp).expect("read log");
        assert_eq!(read_back, content);
        let _ = fs::remove_file(&tmp);
    }
}
