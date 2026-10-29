use std::path::Path;

use crate::error::Result;
use crate::shell::run_command;
use crate::types::package_version::{BumpOperation, RustcPackageVersion, apply_bump_operation};
use crate::types::versions::RustVersion;

/// Run `dch -v <version_str>` to open a new changelog entry.
pub async fn run_dch(repo_dir: &Path, version_str: &str) -> Result<()> {
    // --no-auto-nmu   avoids NMU version mangling.
    // --force-bad-version (-b) allows backport versions that sort lower than
    //   the current entry, which is always the case when backporting to an
    //   older Ubuntu series.
    run_command(
        "dch",
        &["-v", version_str, "--no-auto-nmu", "--force-bad-version"],
        repo_dir,
        // Use `touch` as the editor: it updates the file's mtime (which dch
        // checks to decide whether the editor modified the file) without
        // altering any content.  Using `true` would leave mtime unchanged,
        // causing dch to treat the file as unmodified and roll back the new
        // entry.
        &[("VISUAL", "touch"), ("EDITOR", "touch")],
    )
    .await?;
    Ok(())
}

/// Update the first changelog entry in `debian/changelog` so that:
/// - The source package name is changed from `old_pkg_name` to `new_pkg_name`.
/// - The target distribution is set to `release`.
/// - The first bullet in the changelog body is replaced with the canonical new
///   upstream version entry referencing `lp_bug`.
///
/// This function edits the file in-place using string manipulation.
pub fn update_changelog_entry(
    changelog_path: &Path,
    old_pkg_name: &str,
    new_pkg_name: &str,
    release: &str,
    upstream_version: &str,
    lp_bug: &str,
) -> Result<()> {
    let content = std::fs::read_to_string(changelog_path)?;
    let mut lines: Vec<String> = content.lines().map(|l| l.to_owned()).collect();

    if lines.is_empty() {
        return Ok(());
    }

    // Fix the first line: rename the package and set the distribution.
    //
    // Typical first line format:
    //   rustc-1.84 (1.84.0+dfsg-0ubuntu1) UNRELEASED; urgency=medium
    let first = &lines[0];
    let updated_first = first
        .replace(old_pkg_name, new_pkg_name)
        .replace("UNRELEASED", release);
    lines[0] = updated_first;

    // Replace the first non-empty bullet line in the body with the canonical
    // new upstream version entry.
    let new_bullet = format!("  * New upstream version {upstream_version} (LP: #{lp_bug})");
    for line in lines.iter_mut().skip(1) {
        let trimmed = line.trim();
        if trimmed.starts_with('*') || trimmed.starts_with('-') {
            *line = new_bullet.clone();
            break;
        }
        // Stop if we hit the trailer line (starts with " -- ").
        if line.starts_with(" -- ") {
            break;
        }
    }

    let new_content = lines.join("\n") + "\n";
    std::fs::write(changelog_path, new_content)?;
    Ok(())
}

/// Return the Debian version string for the new upstream Rust release.
///
/// Format: `"<X.Y.Z>+dfsg-0ubuntu1"`
pub fn debian_version_string(version: &RustVersion) -> String {
    format!("{version}+dfsg-0ubuntu1")
}

/// Read the current version string from the first line of `debian/changelog`.
///
/// The first line has the format:
/// `<package> (<version>) <distribution>; urgency=<level>`
///
/// Returns an error if the file cannot be read or the format is unexpected.
pub fn read_current_version(changelog_path: &Path) -> Result<String> {
    let content = std::fs::read_to_string(changelog_path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            // Missing changelog almost always means "not a Debian source
            // package" — say that instead of a bare IO error.
            crate::error::ThermiteError::MissingChangelog(changelog_path.display().to_string())
        } else {
            crate::error::ThermiteError::Io(e)
        }
    })?;
    let first_line = content.lines().next().unwrap_or("");
    // Extract the version between the first `(` and `)`.
    let version = first_line
        .split_once('(')
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(ver, _)| ver.trim().to_owned())
        .unwrap_or_default();
    if version.is_empty() {
        return Err(crate::error::ThermiteError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("could not parse version from debian/changelog first line: '{first_line}'"),
        )));
    }
    Ok(version)
}

/// Compute the Debian version string for a backport to `target_series`.
///
/// Uses the structured [`RustcPackageVersion`] parser to decompose the current
/// version, then applies the backport transformation (sets the target series
/// and resets backport-specific fields).
///
/// Examples (from the backporting docs):
/// - `"1.93.0+dfsg-0ubuntu1"` + `"24.04"`
///   → `"1.93.0+dfsg~24.04-0ubuntu1~24.04.1"`
/// - `"1.89.0+dfsg2~24.04.1-0ubuntu3~24.04.2"` + `"22.04"`
///   → `"1.89.0+dfsg2~22.04-0ubuntu3~22.04.1"`
pub fn compute_backport_version(current_version: &str, target_series: &str) -> String {
    match RustcPackageVersion::parse(current_version) {
        Ok(mut v) => {
            v.to_backport(target_series);
            v.to_string()
        }
        Err(_) => {
            // Fallback for versions that don't fully conform to the new schema
            // (e.g. legacy format). Use the old string-manipulation approach.
            compute_backport_version_legacy(current_version, target_series)
        }
    }
}

/// Legacy fallback for computing a backport version via string manipulation.
///
/// This handles version strings that don't conform to the new schema (e.g.
/// older legacy format packages). The algorithm:
/// 1. Split on the first `-` to get upstream + debian revision.
/// 2. Strip any trailing `~XX.YY[.Z…]` suffix from both parts.
/// 3. Append `~<target_series>` and `~<target_series>.1` respectively.
fn compute_backport_version_legacy(current_version: &str, target_series: &str) -> String {
    let (upstream_raw, debian_rev_raw) = match current_version.split_once('-') {
        Some(pair) => pair,
        None => {
            return format!("{current_version}~{target_series}-0ubuntu1~{target_series}.1");
        }
    };

    let upstream = strip_series_suffix(upstream_raw);
    let debian_rev = strip_series_suffix(debian_rev_raw);

    format!("{upstream}~{target_series}-{debian_rev}~{target_series}.1")
}

/// Strip a trailing `~XX.YY[.Z…]` series suffix from a version component.
///
/// Matches patterns like `~22.04`, `~24.04.1`, `~24.04.2`, etc.
fn strip_series_suffix(s: &str) -> &str {
    // Find the last `~` in the string.
    if let Some(tilde_pos) = s.rfind('~') {
        let suffix = &s[tilde_pos + 1..];
        // The suffix must look like a series: digits, dots, and digits only.
        // Valid: "22.04", "24.04.1", "24.04.2"
        // Invalid: anything else (e.g. a pre-release marker)
        if is_series_like(suffix) {
            return &s[..tilde_pos];
        }
    }
    s
}

/// Return `true` if `s` looks like a numeric series identifier such as
/// `"22.04"` or `"24.04.1"`.
fn is_series_like(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    // Must consist only of ASCII digits and dots, and start/end with a digit.
    let all_digit_dot = s.chars().all(|c| c.is_ascii_digit() || c == '.');
    let starts_digit = s.starts_with(|c: char| c.is_ascii_digit());
    let ends_digit = s.ends_with(|c: char| c.is_ascii_digit());
    all_digit_dot && starts_digit && ends_digit
}

/// Update the most-recent `debian/changelog` entry for a backport so that:
/// - The distribution is set to `release`.
/// - The first bullet is set to `"* Backport to <release> (LP: #N)"` when a
///   bug number is provided, or `"* Backport to <release>"` otherwise.
///
/// This function edits the file in-place.
pub fn update_backport_changelog_entry(
    changelog_path: &Path,
    release: &str,
    lp_bug: Option<&str>,
) -> Result<()> {
    let content = std::fs::read_to_string(changelog_path)?;
    let mut lines: Vec<String> = content.lines().map(|l| l.to_owned()).collect();

    if lines.is_empty() {
        return Ok(());
    }

    // Fix the first line: replace UNRELEASED (or any distribution) with the
    // target release.  The first line format is:
    //   <pkg> (<version>) <distribution>; urgency=medium
    {
        let first = lines[0].clone();
        // Replace the distribution token, which sits between `) ` and `;`.
        if let Some((before_dist, after_semi)) = first
            .split_once(") ")
            .and_then(|(left, right)| right.split_once(';').map(|(_, tail)| (left, tail)))
        {
            lines[0] = format!("{before_dist}) {release};{after_semi}");
        }
    }

    // Build the backport bullet text.
    let bullet = match lp_bug {
        Some(bug) => format!("  * Backport to {release} (LP: #{bug})"),
        None => format!("  * Backport to {release}"),
    };

    // Replace the first non-empty bullet in the body.
    for line in lines.iter_mut().skip(1) {
        let trimmed = line.trim();
        if trimmed.starts_with('*') || trimmed.starts_with('-') {
            *line = bullet.clone();
            break;
        }
        if line.starts_with(" -- ") {
            break;
        }
    }

    let new_content = lines.join("\n") + "\n";
    std::fs::write(changelog_path, new_content)?;
    Ok(())
}

/// Rewrite the version in the first line of `debian/changelog` with the
/// result of applying `operation` to the current top-entry version.
///
/// The top entry is edited **in place** (no new entry is created) — this is
/// what the TUI's "bump version" action means: e.g. bumping the Ubuntu
/// revision before a rebuild should not add a new changelog entry.
///
/// Returns the new version string.
///
/// # Errors
/// - `read_current_version` errors (unreadable/unparsable changelog).
/// - The current version does not parse as a [`RustcPackageVersion`].
/// - `apply_bump_operation` rejects the operation (e.g. backport without a
///   series).
pub fn apply_version_bump(changelog_path: &Path, operation: &BumpOperation) -> Result<String> {
    let current = read_current_version(changelog_path)?;
    let mut v = RustcPackageVersion::parse(&current)?;
    apply_bump_operation(&mut v, operation)?;
    let new_version = v.to_string();
    rewrite_top_entry_version(changelog_path, &current, &new_version)?;
    Ok(new_version)
}

/// Rewrite the version in the first line of `debian/changelog` to
/// `version_str` (the TUI's "set version" action).
///
/// The version string is validated with [`RustcPackageVersion::parse`]
/// before the file is touched.  Returns the version actually written.
pub fn set_changelog_version(changelog_path: &Path, version_str: &str) -> Result<String> {
    let current = read_current_version(changelog_path)?;
    // Validate before touching the file.
    RustcPackageVersion::parse(version_str)?;
    rewrite_top_entry_version(changelog_path, &current, version_str)?;
    Ok(version_str.to_owned())
}

/// Replace the `(old_version)` token in the first line of the changelog with
/// `(new_version)`.
///
/// Both strings are matched literally against the first line, which avoids
/// any regex dependency.  The file is left untouched when the old version
/// cannot be found (defensive; `read_current_version` already guarantees the
/// first line contains a parenthesised version).
fn rewrite_top_entry_version(
    changelog_path: &Path,
    old_version: &str,
    new_version: &str,
) -> Result<()> {
    let content = std::fs::read_to_string(changelog_path)?;
    let mut lines: Vec<String> = content.lines().map(|l| l.to_owned()).collect();

    if lines.is_empty() {
        return Ok(());
    }

    let token = format!("({old_version})");
    let replacement = format!("({new_version})");
    if let Some(pos) = lines[0].find(&token) {
        lines[0].replace_range(pos..pos + token.len(), &replacement);
    }

    let new_content = lines.join("\n") + "\n";
    std::fs::write(changelog_path, new_content)?;
    Ok(())
}

#[cfg(test)]
mod backport_changelog_tests {
    use super::*;

    #[test]
    fn compute_backport_version_from_devel() {
        // Devel (no series number in orig part) → Noble
        assert_eq!(
            compute_backport_version("1.93.0+dfsg-0ubuntu1", "24.04"),
            "1.93.0+dfsg~24.04-0ubuntu1~24.04.1"
        );
    }

    #[test]
    fn compute_backport_version_from_stable_to_older() {
        // Noble → Jammy
        assert_eq!(
            compute_backport_version("1.89.0+dfsg2~24.04.1-0ubuntu3~24.04.2", "22.04"),
            "1.89.0+dfsg2~22.04-0ubuntu3~22.04.1"
        );
    }

    #[test]
    fn compute_backport_version_strips_revision_series() {
        // When the debian revision already carries a series suffix it is stripped.
        assert_eq!(
            compute_backport_version("1.85.0+dfsg~25.04-0ubuntu1~25.04.1", "24.04"),
            "1.85.0+dfsg~24.04-0ubuntu1~24.04.1"
        );
    }

    #[test]
    fn compute_backport_version_idempotent_for_same_series() {
        // When the input already targets the same series, the computed version
        // must equal the input.  backport.rs Phase 3 relies on this invariant
        // to decide whether to skip `dch -v` and avoid creating a duplicate
        // changelog entry on re-runs.
        let v = "1.94.1+dfsg~24.04-0ubuntu1~24.04.1";
        assert_eq!(compute_backport_version(v, "24.04"), v);

        // Also idempotent across different version shapes targeting the same
        // series (e.g. a multi-segment debian revision).
        let v2 = "1.89.0+dfsg2~22.04-0ubuntu3~22.04.1";
        assert_eq!(compute_backport_version(v2, "22.04"), v2);
    }

    #[test]
    fn strip_series_suffix_with_sub_revision() {
        assert_eq!(strip_series_suffix("0ubuntu3~24.04.2"), "0ubuntu3");
        assert_eq!(strip_series_suffix("1.89.0+dfsg2~24.04.1"), "1.89.0+dfsg2");
    }

    #[test]
    fn strip_series_suffix_without_suffix() {
        assert_eq!(strip_series_suffix("0ubuntu1"), "0ubuntu1");
        assert_eq!(strip_series_suffix("1.93.0+dfsg"), "1.93.0+dfsg");
    }

    #[test]
    fn is_series_like_valid() {
        assert!(is_series_like("22.04"));
        assert!(is_series_like("24.04.1"));
        assert!(is_series_like("25.10"));
    }

    #[test]
    fn is_series_like_invalid() {
        assert!(!is_series_like(""));
        assert!(!is_series_like("alpha"));
        assert!(!is_series_like("24.04-foo"));
    }

    // ------------------------------------------------------------------
    // apply_version_bump / set_changelog_version (TUI helpers)
    // ------------------------------------------------------------------

    const SAMPLE_CHANGELOG: &str = "\
rustc-1.95 (1.95.0+dfsg-0ubuntu1) plucky; urgency=medium

  * New upstream version 1.95.0

 -- Jane Doe <jane@example.com>  Mon, 01 Jan 2026 00:00:00 +0000
";

    /// Create a uniquely named scratch directory (project convention: no
    /// tempfile dev-dependency).
    fn make_scratch_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "thermite-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_sample_changelog(dir: &std::path::Path) -> std::path::PathBuf {
        let path = dir.join("changelog");
        std::fs::write(&path, SAMPLE_CHANGELOG).unwrap();
        path
    }

    #[test]
    fn read_current_version_missing_changelog_is_clear() {
        // Not a Debian source package → the error must say so, not surface a
        // bare "No such file or directory (os error 2)".
        let dir = make_scratch_dir("missing-changelog");
        let path = dir.join("debian/changelog");
        let err = read_current_version(&path).unwrap_err();
        assert!(matches!(
            err,
            crate::error::ThermiteError::MissingChangelog(_)
        ));
        let msg = err.to_string();
        assert!(
            msg.contains("not a Debian source package") && msg.contains("missing debian/changelog"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn apply_version_bump_ubuntu_revision() {
        let dir = make_scratch_dir("bump-ubuntu-rev");
        let path = write_sample_changelog(&dir);

        let new_version = apply_version_bump(&path, &BumpOperation::UbuntuRevision).unwrap();
        assert_eq!(new_version, "1.95.0+dfsg-0ubuntu2");

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.starts_with("rustc-1.95 (1.95.0+dfsg-0ubuntu2) plucky; urgency=medium"));
        // The rest of the entry must be untouched.
        assert!(content.contains("  * New upstream version 1.95.0"));
        assert!(content.contains(" -- Jane Doe <jane@example.com>"));
    }

    #[test]
    fn apply_version_bump_backport_with_release_adjective() {
        let dir = make_scratch_dir("bump-test");
        let path = write_sample_changelog(&dir);

        let new_version = apply_version_bump(
            &path,
            &BumpOperation::Backport {
                series: None,
                release: Some("noble".to_owned()),
            },
        )
        .unwrap();
        assert_eq!(new_version, "1.95.0+dfsg~24.04-0ubuntu1~24.04.1");
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with("rustc-1.95 (1.95.0+dfsg~24.04-0ubuntu1~24.04.1) plucky;")
        );
    }

    #[test]
    fn apply_version_bump_requires_series_for_backport() {
        let dir = make_scratch_dir("bump-test");
        let path = write_sample_changelog(&dir);

        let result = apply_version_bump(
            &path,
            &BumpOperation::Backport {
                series: None,
                release: None,
            },
        );
        assert!(result.is_err());
        // The file must be untouched.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SAMPLE_CHANGELOG);
    }

    #[test]
    fn apply_version_bump_patch_release_resets_fields() {
        let dir = make_scratch_dir("bump-test");
        let path = write_sample_changelog(&dir);

        let new_version = apply_version_bump(
            &path,
            &BumpOperation::PatchRelease {
                upstream: "1.95.1".to_owned(),
            },
        )
        .unwrap();
        assert_eq!(new_version, "1.95.1+dfsg-0ubuntu1");
    }

    #[test]
    fn set_changelog_version_validates_before_writing() {
        let dir = make_scratch_dir("bump-test");
        let path = write_sample_changelog(&dir);

        // Invalid version → error, file untouched.
        let result = set_changelog_version(&path, "1.95.0-0ubuntu1");
        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SAMPLE_CHANGELOG);

        // Valid version → written.
        let written = set_changelog_version(&path, "1.96.0+dfsg-0ubuntu1").unwrap();
        assert_eq!(written, "1.96.0+dfsg-0ubuntu1");
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with("rustc-1.95 (1.96.0+dfsg-0ubuntu1) plucky;")
        );
    }

    #[test]
    fn rewrite_preserves_unreleased_distribution() {
        let dir = make_scratch_dir("bump-unreleased");
        let path = dir.join("changelog");
        std::fs::write(
            &path,
            "\
rustc-1.95 (1.95.0+dfsg-0ubuntu1) UNRELEASED; urgency=medium
",
        )
        .unwrap();

        apply_version_bump(&path, &BumpOperation::UbuntuRevision).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.starts_with("rustc-1.95 (1.95.0+dfsg-0ubuntu2) UNRELEASED;"));
    }
}
