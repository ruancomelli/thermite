//! Compatibility checks for backporting.
//!
//! Each check follows the same 5-step pattern:
//!   1. Infer the required version/dependency from the source packaging.
//!   2. Check whether the required version exists in the target release's archive.
//!   3. If it exists, report a confirmation.
//!   4. If it does not exist (or is too old), report the applicable fix.
//!   5. If inference failed, explicitly state this and provide a manual check URL.
//!
//! Only the known common fixes (LLVM, libgit2, dh-cargo, pkgconf, cmake,
//! debhelper-compat) are detected — no attempt is made to diagnose arbitrary
//! build failures.

use std::path::Path;

use crate::cache;
use crate::error::ThermiteError;
use crate::shell::{run_command, which};

// ── Public types ──────────────────────────────────────────────────────────────

/// The outcome of inferring a required version or dependency from the source
/// packaging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inference {
    /// A value was successfully inferred.
    ///
    /// `value` is the human-readable form (e.g. `"19"`, `"1.9.0~~"`); `source`
    /// names where it was found (e.g. `"debian/rules: LLVM_VERSION = 19"`).
    Inferred { value: String, source: String },
    /// The value could not be determined automatically.
    ///
    /// `reason` explains what was expected and not found.
    CouldNotInfer(String),
}

impl Inference {
    /// Returns `true` when this is an `Inferred` value (regardless of whether
    /// the value string is empty).
    pub fn is_inferred(&self) -> bool {
        matches!(self, Inference::Inferred { .. })
    }
}

/// The status of a package/version in the target release's archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveStatus {
    /// The package is published in the archive; carries its version string.
    Available(String),
    /// The package is published but older than the required version.
    TooOld { available: String, required: String },
    /// The package is not published in the archive at all.
    NotPublished,
    /// The archive check itself failed (e.g. `rmadison` not installed,
    /// network error).
    CheckFailed(String),
}

/// The combined result of a single compatibility check.
#[derive(Debug, Clone)]
pub struct CheckResult {
    /// Short human-readable name of the check (e.g. `"LLVM"`).
    pub name: &'static str,
    /// What thermite inferred from the source packaging.
    pub inference: Inference,
    /// What thermite found (or failed to find) in the archive.
    pub archive_status: ArchiveStatus,
    /// One-line summary of the applicable fix, or empty when no fix is needed.
    pub guidance: &'static str,
    /// Launchpad URL for manual verification.
    pub url: String,
}

impl CheckResult {
    /// Returns `true` when the check passed and no action is needed.
    pub fn is_ok(&self) -> bool {
        matches!(self.archive_status, ArchiveStatus::Available(_))
    }
}

// ── Inference helpers ─────────────────────────────────────────────────────────

/// Read a file relative to `repo_dir`, returning an empty string on missing
/// file so inference functions can treat "file not present" as "could not
/// infer" uniformly.
fn read_debian_file(repo_dir: &Path, name: &str) -> String {
    std::fs::read_to_string(repo_dir.join(name)).unwrap_or_default()
}

/// Iterate over the lines of `content`, skipping lines whose first non-space
/// character is `#`. Used by inference functions so commented-out directives
/// (common in backport branches) are not mistaken for active ones.
fn uncommented_lines(content: &str) -> impl Iterator<Item = &str> {
    content.lines().filter(|line| {
        let t = line.trim_start();
        !t.starts_with('#')
    })
}

/// Infer the LLVM major version required by this `rustc` package by scanning
/// `debian/rules` for an uncommented `LLVM_VERSION = <N>` line.
pub fn infer_llvm_version(repo_dir: &Path) -> Inference {
    let content = read_debian_file(repo_dir, "debian/rules");
    for line in uncommented_lines(&content) {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("LLVM_VERSION") {
            // Accept optional surrounding whitespace and a '=' or ':' separator.
            let rest = rest.trim_start_matches([' ', '=', ':', '\t']).trim_end();
            if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit() || c == '.') {
                let value = rest.to_owned();
                return Inference::Inferred {
                    value: value.clone(),
                    source: format!("debian/rules: LLVM_VERSION = {value}"),
                };
            }
        }
    }
    Inference::CouldNotInfer(
        "no uncommented 'LLVM_VERSION = <N>' assignment found in debian/rules".to_owned(),
    )
}

/// Infer the libgit2-dev minimum version from `debian/control` by looking for
/// `libgit2-dev (>= X.Y.Z` in Build-Depends.
pub fn infer_libgit2_version(repo_dir: &Path) -> Inference {
    let content = read_debian_file(repo_dir, "debian/control");
    for line in uncommented_lines(&content) {
        let trimmed = line.trim();
        if let Some(idx) = trimmed.find("libgit2-dev") {
            let after = &trimmed[idx + "libgit2-dev".len()..];
            // Look for `(>= <version>)`.
            if let Some(ver_start) = after.find("(>=") {
                let rest = &after[ver_start + "(>=".len()..];
                if let Some(end) = rest.find(')') {
                    let ver = rest[..end].trim().to_owned();
                    if !ver.is_empty() {
                        return Inference::Inferred {
                            value: ver.clone(),
                            source: format!("debian/control: libgit2-dev (>= {ver})"),
                        };
                    }
                }
            }
            // libgit2-dev present but no version constraint.
            return Inference::Inferred {
                value: String::new(),
                source: "debian/control: libgit2-dev (no version constraint)".to_owned(),
            };
        }
    }
    Inference::CouldNotInfer(
        "no libgit2-dev entry found in debian/control Build-Depends".to_owned(),
    )
}

/// Infer the dh-cargo minimum version from `debian/control`.
///
/// Returns `Some(version)` (possibly empty when no constraint is given) when
/// dh-cargo is present, or `CouldNotInfer` when it is absent.
pub fn infer_dh_cargo(repo_dir: &Path) -> Inference {
    let content = read_debian_file(repo_dir, "debian/control");
    for line in uncommented_lines(&content) {
        let trimmed = line.trim();
        if let Some(idx) = trimmed.find("dh-cargo") {
            let after = &trimmed[idx + "dh-cargo".len()..];
            if let Some(ver_start) = after.find("(>=") {
                let rest = &after[ver_start + "(>=".len()..];
                if let Some(end) = rest.find(')') {
                    let ver = rest[..end].trim().to_owned();
                    return Inference::Inferred {
                        value: ver.clone(),
                        source: format!("debian/control: dh-cargo (>= {ver})"),
                    };
                }
            }
            // dh-cargo present without version constraint.
            return Inference::Inferred {
                value: String::new(),
                source: "debian/control: dh-cargo (no version constraint)".to_owned(),
            };
        }
    }
    Inference::CouldNotInfer("no dh-cargo entry found in debian/control".to_owned())
}

/// Infer whether pkgconf is used as a Build-Depends in `debian/control`.
///
/// Returns `Inferred { value: "present" }` when pkgconf is found, or
/// `Inferred { value: "pkg-config" }` when pkg-config is already used instead,
/// or `CouldNotInfer` when neither is present.
pub fn infer_pkgconf(repo_dir: &Path) -> Inference {
    let content = read_debian_file(repo_dir, "debian/control");
    for line in uncommented_lines(&content) {
        let trimmed = line.trim();
        // Match `pkgconf` as a whole word, not `pkg-config`.
        // Use a simple heuristic: look for `pkgconf` followed by whitespace,
        // comma, or end-of-line.
        if contains_word(trimmed, "pkgconf") && !contains_word(trimmed, "pkg-config") {
            return Inference::Inferred {
                value: "present".to_owned(),
                source: "debian/control: pkgconf in Build-Depends".to_owned(),
            };
        }
        if contains_word(trimmed, "pkg-config") {
            return Inference::Inferred {
                value: "pkg-config".to_owned(),
                source: "debian/control: pkg-config in Build-Depends".to_owned(),
            };
        }
    }
    Inference::CouldNotInfer("neither pkgconf nor pkg-config found in debian/control".to_owned())
}

/// Infer the cmake minimum version from `debian/control`.
pub fn infer_cmake(repo_dir: &Path) -> Inference {
    let content = read_debian_file(repo_dir, "debian/control");
    for line in uncommented_lines(&content) {
        let trimmed = line.trim();
        // Match `cmake` as a whole word, but not `cmake-mozilla` or `cmake3`.
        if let Some(idx) = find_word(trimmed, "cmake") {
            let after = &trimmed[idx + "cmake".len()..];
            // Skip cmake-mozilla / cmake3 — only match bare cmake.
            if after.starts_with('-') || after.starts_with('3') {
                continue;
            }
            if let Some(ver_start) = after.find("(>=") {
                let rest = &after[ver_start + "(>=".len()..];
                if let Some(end) = rest.find(')') {
                    let ver = rest[..end].trim().to_owned();
                    return Inference::Inferred {
                        value: ver.clone(),
                        source: format!("debian/control: cmake (>= {ver})"),
                    };
                }
            }
            return Inference::Inferred {
                value: String::new(),
                source: "debian/control: cmake (no version constraint)".to_owned(),
            };
        }
    }
    Inference::CouldNotInfer("no cmake entry found in debian/control".to_owned())
}

/// Infer the debhelper-compat level from `debian/control`.
pub fn infer_debhelper_compat(repo_dir: &Path) -> Inference {
    let content = read_debian_file(repo_dir, "debian/control");
    for line in uncommented_lines(&content) {
        let trimmed = line.trim();
        if let Some(idx) = trimmed.find("debhelper-compat") {
            let after = &trimmed[idx + "debhelper-compat".len()..];
            // Look for `(= N)` or `(>= N)`.
            if let Some(ver_start) = after.find("(=") {
                let rest = &after[ver_start + "(=".len()..];
                if let Some(end) = rest.find(')') {
                    let ver = rest[..end].trim().to_owned();
                    if !ver.is_empty() {
                        return Inference::Inferred {
                            value: ver.clone(),
                            source: format!("debian/control: debhelper-compat (= {ver})"),
                        };
                    }
                }
            }
            if let Some(ver_start) = after.find("(>=") {
                let rest = &after[ver_start + "(>=".len()..];
                if let Some(end) = rest.find(')') {
                    let ver = rest[..end].trim().to_owned();
                    if !ver.is_empty() {
                        return Inference::Inferred {
                            value: ver.clone(),
                            source: format!("debian/control: debhelper-compat (>= {ver})"),
                        };
                    }
                }
            }
        }
    }
    Inference::CouldNotInfer("no debhelper-compat entry found in debian/control".to_owned())
}

// ── Word-boundary helpers ─────────────────────────────────────────────────────

/// Returns `true` when `word` appears in `line` as a whole word (i.e. not
/// preceded or followed by a non-whitespace, non-comma, non-paren character).
fn contains_word(line: &str, word: &str) -> bool {
    find_word(line, word).is_some()
}

/// Find the byte index of `word` in `line` when it appears as a whole word.
/// Returns `None` when not found.
fn find_word(line: &str, word: &str) -> Option<usize> {
    let mut start = 0;
    while let Some(idx) = line[start..].find(word) {
        let abs = start + idx;
        let before_ok = abs
            .checked_sub(1)
            .map(|i| is_word_boundary(line.as_bytes()[i]))
            .unwrap_or(true);
        let after_idx = abs + word.len();
        let after_ok = line
            .as_bytes()
            .get(after_idx)
            .map(|b| is_word_boundary(*b))
            .unwrap_or(true);
        if before_ok && after_ok {
            return Some(abs);
        }
        start = abs + word.len();
    }
    None
}

/// A byte is a word boundary for our purposes when it is whitespace, a comma,
/// a parenthesis, or end-of-line.
fn is_word_boundary(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b',' | b'(' | b')' | b'|')
}

// ── Archive check ─────────────────────────────────────────────────────────────

/// Check whether `package` is published in the `release` suite of the Ubuntu
/// archive using `rmadison -u ubuntu <package>`.
///
/// The raw rmadison output is cached per query under the user cache directory
/// (see [`crate::cache`]); repeated backports against the same query reuse the
/// stored result instead of re-querying the archive. A cache hit needs no
/// local `rmadison` binary, so the availability check only guards the
/// network-fetch path.
///
/// Returns the version string of the first matching entry, or `NotPublished`
/// when no entry matches the release.
pub async fn check_archive(package: &str, release: &str) -> ArchiveStatus {
    if let Some(hit) = cache::lookup_rmadison(package) {
        println!(
            "  rmadison: using cached result for {package} ({})",
            cache::format_age(hit.age_secs)
        );
        return match parse_rmadison_version(&hit.data, release) {
            Some(v) => ArchiveStatus::Available(v),
            None => ArchiveStatus::NotPublished,
        };
    }
    if which("rmadison").is_err() {
        return ArchiveStatus::CheckFailed(
            "rmadison not installed (sudo apt install devscripts)".to_owned(),
        );
    }
    let output =
        match run_command("rmadison", &["-u", "ubuntu", package], Path::new("."), &[]).await {
            Ok(o) => o,
            Err(ThermiteError::CommandFailed { stdout, stderr, .. }) => {
                // rmadison exits non-zero on network failure; surface a hint.
                let detail = if !stderr.trim().is_empty() {
                    stderr.trim().to_owned()
                } else if !stdout.trim().is_empty() {
                    stdout.trim().to_owned()
                } else {
                    "rmadison exited non-zero".to_owned()
                };
                return ArchiveStatus::CheckFailed(detail);
            }
            Err(e) => return ArchiveStatus::CheckFailed(e.to_string()),
        };
    cache::store_rmadison(package, &output.stdout);
    match parse_rmadison_version(&output.stdout, release) {
        Some(v) => ArchiveStatus::Available(v),
        None => ArchiveStatus::NotPublished,
    }
}

/// Parse `rmadison -u ubuntu <package>` output and return the version string
/// of the first entry whose suite matches `release`.
///
/// rmadison output format (one source per line, pipe-separated):
/// ```text
///   rustc | 1.85.0+dfsg1~24.04-0ubuntu1~24.04.1 | noble-security | source
///   rustc | 1.85.0+dfsg1~24.04-0ubuntu1~24.04.1 | noble-updates   | source
///   rustc | 1.85.0+dfsg1-0ubuntu1               | noble           | source
/// ```
///
/// The suite field is emitted as `<suite>` or `<suite>/<component>`
/// (e.g. `noble` or `resolute/universe`), and may carry a pocket suffix
/// (e.g. `noble-security`, `noble-updates`, `resolute-proposed/universe`).
///
/// We accept entries whose suite equals `release`, starts with
/// `<release>-` (e.g. `jammy-security`, `jammy-updates`), or starts with
/// `<release>/` (e.g. `resolute/universe`). Iteration preserves rmadison's
/// output order, so the first matching line wins — callers that need the
/// newest version across pockets should pre-sort the input.
pub fn parse_rmadison_version(stdout: &str, release: &str) -> Option<String> {
    let with_hyphen = format!("{release}-");
    let with_slash = format!("{release}/");
    for line in stdout.lines() {
        // Skip blank lines, warnings, and headers.
        if line.trim().is_empty() || !line.contains('|') {
            continue;
        }
        let parts: Vec<&str> = line.split('|').map(|p| p.trim()).collect();
        if parts.len() < 4 {
            continue;
        }
        // parts[0] = package, parts[1] = version, parts[2] = suite, parts[3+] = archs
        let version = parts[1];
        let suite = parts[2];
        if suite == release || suite.starts_with(&with_hyphen) || suite.starts_with(&with_slash) {
            return Some(version.to_owned());
        }
    }
    None
}

/// Compare two Debian version strings using `dpkg --compare-versions`.
///
/// `op` is one of `lt`, `le`, `eq`, `ne`, `ge`, `gt`. Returns `true` when the
/// comparison holds. On any error (e.g. dpkg unavailable), returns `false`.
pub async fn dpkg_compare_versions(a: &str, op: &str, b: &str) -> bool {
    let result = run_command(
        "dpkg",
        &["--compare-versions", a, op, b],
        Path::new("."),
        &[],
    )
    .await;
    match result {
        Ok(o) => o.status.success(),
        Err(ThermiteError::CommandFailed { .. }) => false,
        Err(_) => false,
    }
}

// ── Per-gate check functions ──────────────────────────────────────────────────

/// Construct a Launchpad source package URL for `package` in `release`.
fn lp_source_url(release: &str, package: &str) -> String {
    format!("https://launchpad.net/ubuntu/{release}/+source/{package}")
}

/// Check whether the LLVM version required by the package is available in the
/// target release's archive.
pub async fn check_llvm(repo_dir: &Path, release: &str) -> CheckResult {
    let inference = infer_llvm_version(repo_dir);
    let guidance = "Vendor LLVM: remove src/llvm-project from Files-Excluded \
                    in debian/copyright, regenerate the orig tarball, update \
                    debian/control and debian/config.toml.in per the backporting guide.";

    match &inference {
        Inference::Inferred { value, .. } => {
            let pkg = format!("llvm-toolchain-{value}");
            let url = lp_source_url(release, &pkg);
            let status = check_archive(&pkg, release).await;
            CheckResult {
                name: "LLVM",
                inference,
                archive_status: status,
                guidance,
                url,
            }
        }
        Inference::CouldNotInfer(reason) => {
            let url = format!("https://launchpad.net/ubuntu/{release}/+source/llvm-toolchain-<N>");
            CheckResult {
                name: "LLVM",
                inference: Inference::CouldNotInfer(reason.clone()),
                archive_status: ArchiveStatus::CheckFailed(reason.clone()),
                guidance,
                url,
            }
        }
    }
}

/// Check whether the libgit2-dev version required by the package is available
/// in the target release's archive.
pub async fn check_libgit2(repo_dir: &Path, release: &str) -> CheckResult {
    let inference = infer_libgit2_version(repo_dir);
    let guidance = "Either downgrade the libgit2-dev version constraint in \
                    debian/control and debian/control.in (when the archive \
                    version is API-compatible), or vendor libgit2 (comment \
                    out the libgit2 exclusion in debian/copyright, regenerate \
                    the orig tarball, remove libgit2-dev from Build-Depends).";

    match &inference {
        Inference::Inferred { value, source } => {
            let url = lp_source_url(release, "libgit2");
            if value.is_empty() {
                // No version constraint — treat as available if the package is
                // published at all.
                let status = check_archive("libgit2", release).await;
                return CheckResult {
                    name: "libgit2",
                    inference: Inference::Inferred {
                        value: value.clone(),
                        source: source.clone(),
                    },
                    archive_status: status,
                    guidance,
                    url,
                };
            }
            let status = check_archive("libgit2", release).await;
            let final_status = match status {
                ArchiveStatus::Available(available) => {
                    let required_norm = value.trim_end_matches('~');
                    if dpkg_compare_versions(&available, "ge", required_norm).await {
                        ArchiveStatus::Available(available)
                    } else {
                        ArchiveStatus::TooOld {
                            available,
                            required: value.clone(),
                        }
                    }
                }
                other => other,
            };
            CheckResult {
                name: "libgit2",
                inference: Inference::Inferred {
                    value: value.clone(),
                    source: source.clone(),
                },
                archive_status: final_status,
                guidance,
                url,
            }
        }
        Inference::CouldNotInfer(reason) => CheckResult {
            name: "libgit2",
            inference: Inference::CouldNotInfer(reason.clone()),
            archive_status: ArchiveStatus::CheckFailed(reason.clone()),
            guidance,
            url: lp_source_url(release, "libgit2"),
        },
    }
}

/// Check whether dh-cargo (>= 28ubuntu1~) is available in the target release.
pub async fn check_dh_cargo(repo_dir: &Path, release: &str) -> CheckResult {
    let inference = infer_dh_cargo(repo_dir);
    let guidance = "Comment out dh-cargo from Build-Depends in debian/control \
                    and debian/control.in; remove the dh-cargo-vendored-sources \
                    check from debian/rules.";

    match &inference {
        Inference::Inferred { value, source } => {
            let url = lp_source_url(release, "dh-cargo");
            let status = check_archive("dh-cargo", release).await;
            let final_status = if value.is_empty() {
                status
            } else {
                match status {
                    ArchiveStatus::Available(available) => {
                        if dpkg_compare_versions(&available, "ge", value).await {
                            ArchiveStatus::Available(available)
                        } else {
                            ArchiveStatus::TooOld {
                                available,
                                required: value.clone(),
                            }
                        }
                    }
                    other => other,
                }
            };
            CheckResult {
                name: "dh-cargo",
                inference: Inference::Inferred {
                    value: value.clone(),
                    source: source.clone(),
                },
                archive_status: final_status,
                guidance,
                url,
            }
        }
        Inference::CouldNotInfer(reason) => CheckResult {
            name: "dh-cargo",
            inference: Inference::CouldNotInfer(reason.clone()),
            archive_status: ArchiveStatus::CheckFailed(reason.clone()),
            guidance,
            url: lp_source_url(release, "dh-cargo"),
        },
    }
}

/// Check whether pkgconf is available in the target release's archive.
pub async fn check_pkgconf(repo_dir: &Path, release: &str) -> CheckResult {
    let inference = infer_pkgconf(repo_dir);
    let guidance = "Replace pkgconf with pkg-config in debian/control and \
                    debian/control.in; add 'export PKG_CONFIG=pkg-config' to \
                    debian/rules.";

    match &inference {
        Inference::Inferred { value, source } => {
            // When pkg-config is already used, no action is needed.
            if value == "pkg-config" {
                return CheckResult {
                    name: "pkgconf",
                    inference: Inference::Inferred {
                        value: value.clone(),
                        source: source.clone(),
                    },
                    archive_status: ArchiveStatus::Available(
                        "pkg-config (already in use)".to_owned(),
                    ),
                    guidance: "",
                    url: lp_source_url(release, "pkgconf"),
                };
            }
            let url = lp_source_url(release, "pkgconf");
            let status = check_archive("pkgconf", release).await;
            CheckResult {
                name: "pkgconf",
                inference: Inference::Inferred {
                    value: value.clone(),
                    source: source.clone(),
                },
                archive_status: status,
                guidance,
                url,
            }
        }
        Inference::CouldNotInfer(reason) => CheckResult {
            name: "pkgconf",
            inference: Inference::CouldNotInfer(reason.clone()),
            archive_status: ArchiveStatus::CheckFailed(reason.clone()),
            guidance,
            url: lp_source_url(release, "pkgconf"),
        },
    }
}

/// Check whether cmake (>= 3.0) is available in the target release's archive.
pub async fn check_cmake(repo_dir: &Path, release: &str) -> CheckResult {
    let inference = infer_cmake(repo_dir);
    let guidance = "Add cmake-mozilla (>= 3.0) as a fallback cmake provider in \
                    debian/control and debian/control.in.";

    match &inference {
        Inference::Inferred { value, source } => {
            let url = lp_source_url(release, "cmake");
            let status = check_archive("cmake", release).await;
            let final_status = if value.is_empty() {
                status
            } else {
                match status {
                    ArchiveStatus::Available(available) => {
                        if dpkg_compare_versions(&available, "ge", value).await {
                            ArchiveStatus::Available(available)
                        } else {
                            ArchiveStatus::TooOld {
                                available,
                                required: value.clone(),
                            }
                        }
                    }
                    other => other,
                }
            };
            CheckResult {
                name: "cmake",
                inference: Inference::Inferred {
                    value: value.clone(),
                    source: source.clone(),
                },
                archive_status: final_status,
                guidance,
                url,
            }
        }
        Inference::CouldNotInfer(reason) => CheckResult {
            name: "cmake",
            inference: Inference::CouldNotInfer(reason.clone()),
            archive_status: ArchiveStatus::CheckFailed(reason.clone()),
            guidance,
            url: lp_source_url(release, "cmake"),
        },
    }
}

/// Check whether the required debhelper-compat level is available in the
/// target release's archive.
pub async fn check_debhelper_compat(repo_dir: &Path, release: &str) -> CheckResult {
    let inference = infer_debhelper_compat(repo_dir);
    let guidance = "Downgrade the debhelper-compat level in debian/control and \
                    debian/control.in, and update .install.in substitution \
                    variables to match the lower compat level.";

    match &inference {
        Inference::Inferred { value, source } => {
            let url = lp_source_url(release, "debhelper");
            let status = check_archive("debhelper", release).await;
            let final_status = match status {
                ArchiveStatus::Available(available) => {
                    // Compare the debhelper archive version against the
                    // required compat level (treated as a version floor).
                    if dpkg_compare_versions(&available, "ge", value).await {
                        ArchiveStatus::Available(available)
                    } else {
                        ArchiveStatus::TooOld {
                            available,
                            required: value.clone(),
                        }
                    }
                }
                other => other,
            };
            CheckResult {
                name: "debhelper-compat",
                inference: Inference::Inferred {
                    value: value.clone(),
                    source: source.clone(),
                },
                archive_status: final_status,
                guidance,
                url,
            }
        }
        Inference::CouldNotInfer(reason) => CheckResult {
            name: "debhelper-compat",
            inference: Inference::CouldNotInfer(reason.clone()),
            archive_status: ArchiveStatus::CheckFailed(reason.clone()),
            guidance,
            url: lp_source_url(release, "debhelper"),
        },
    }
}

/// Run all six compatibility checks and return the results in display order.
pub async fn run_all_checks(repo_dir: &Path, release: &str) -> Vec<CheckResult> {
    vec![
        check_llvm(repo_dir, release).await,
        check_libgit2(repo_dir, release).await,
        check_dh_cargo(repo_dir, release).await,
        check_pkgconf(repo_dir, release).await,
        check_cmake(repo_dir, release).await,
        check_debhelper_compat(repo_dir, release).await,
    ]
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_repo() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "thermite-compat-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        ));
        fs::create_dir_all(dir.join("debian")).unwrap();
        dir
    }

    fn write(repo: &std::path::Path, name: &str, content: &str) {
        fs::write(repo.join(name), content).unwrap();
    }

    // ── parse_rmadison_version ─────────────────────────────────────────────

    #[test]
    fn parse_rmadison_version_picks_matching_suite() {
        let out = "\
rustc | 1.85.0+dfsg1~24.04-0ubuntu1~24.04.1 | noble-security | source
rustc | 1.85.0+dfsg1~24.04-0ubuntu1~24.04.1 | noble-updates   | source
rustc | 1.85.0+dfsg1-0ubuntu1               | noble           | source
";
        let v = parse_rmadison_version(out, "noble");
        assert_eq!(v.as_deref(), Some("1.85.0+dfsg1~24.04-0ubuntu1~24.04.1"));
    }

    #[test]
    fn parse_rmadison_version_returns_none_when_not_published() {
        let out = "\
rustc | 1.85.0+dfsg1-0ubuntu1 | oracular | source
";
        assert!(parse_rmadison_version(out, "jammy").is_none());
    }

    #[test]
    fn parse_rmadison_version_skips_blank_and_warning_lines() {
        let out = "\
warning: some network hiccup
\n\
rustc | 1.85.0+dfsg1-0ubuntu1 | jammy | source
";
        let v = parse_rmadison_version(out, "jammy");
        assert_eq!(v.as_deref(), Some("1.85.0+dfsg1-0ubuntu1"));
    }

    #[test]
    fn parse_rmadison_version_handles_empty_output() {
        assert!(parse_rmadison_version("", "jammy").is_none());
    }

    #[test]
    fn parse_rmadison_version_handles_component_suffix() {
        // rmadison emits the suite as `<suite>/<component>` rather than a bare
        // suite name (e.g. `resolute/universe`, `stonking-proposed/universe`).
        // Regression test for LLVM on Resolute, where the previous parser only
        // matched `<release>` and `<release>-*` and so reported NotPublished.
        let out = "\
llvm-toolchain-22 | 1:22.1.2-1ubuntu1 | resolute/universe          | source
llvm-toolchain-22 | 1:22.1.6-1ubuntu1 | stonking/universe          | source
llvm-toolchain-22 | 1:22.1.6-1ubuntu2 | stonking-proposed/universe | source
";
        let v = parse_rmadison_version(out, "resolute");
        assert_eq!(v.as_deref(), Some("1:22.1.2-1ubuntu1"));
    }

    #[test]
    fn parse_rmadison_version_component_suffix_for_pocket() {
        // A pocket with a component (e.g. `resolute-proposed/universe`)
        // should still match `release = "resolute"` via the `<release>-`
        // rule, regardless of the trailing `/component`.
        let out = "\
llvm-toolchain-22 | 1:22.1.6-1ubuntu2 | resolute-proposed/universe | source
llvm-toolchain-22 | 1:22.1.2-1ubuntu1 | resolute/universe          | source
";
        let v = parse_rmadison_version(out, "resolute");
        assert_eq!(v.as_deref(), Some("1:22.1.6-1ubuntu2"));
    }

    #[test]
    fn parse_rmadison_version_component_suffix_does_not_cross_release() {
        // `stonking/universe` must not match `release = "resolute"`,
        // nor vice versa, even though both carry `/universe`.
        let out = "\
llvm-toolchain-22 | 1:22.1.2-1ubuntu1 | resolute/universe | source
llvm-toolchain-22 | 1:22.1.6-1ubuntu1 | stonking/universe | source
";
        assert_eq!(
            parse_rmadison_version(out, "resolute").as_deref(),
            Some("1:22.1.2-1ubuntu1")
        );
        assert_eq!(
            parse_rmadison_version(out, "stonking").as_deref(),
            Some("1:22.1.6-1ubuntu1")
        );
    }

    // ── infer_llvm_version ─────────────────────────────────────────────────

    #[test]
    fn infer_llvm_version_from_rules() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/rules",
            "#!/usr/bin/make -f\nLLVM_VERSION = 19\nOLD_LLVM_VERSION = 18\n",
        );
        match infer_llvm_version(&repo) {
            Inference::Inferred { value, source } => {
                assert_eq!(value, "19");
                assert!(source.contains("LLVM_VERSION = 19"));
            }
            other => panic!("expected Inferred, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn infer_llvm_version_skips_commented_assignment() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/rules",
            "# # Use system LLVM (comment out to use vendored LLVM)\n# LLVM_VERSION = 19\n",
        );
        assert!(matches!(
            infer_llvm_version(&repo),
            Inference::CouldNotInfer(_)
        ));
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn infer_llvm_version_missing_returns_could_not_infer() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/rules",
            "#!/usr/bin/make -f\nall:\n\techo hi\n",
        );
        assert!(matches!(
            infer_llvm_version(&repo),
            Inference::CouldNotInfer(_)
        ));
        let _ = fs::remove_dir_all(&repo);
    }

    // ── infer_libgit2_version ──────────────────────────────────────────────

    #[test]
    fn infer_libgit2_version_with_constraint() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/control",
            "Source: rustc-1.85\nBuild-Depends:\n libgit2-dev (>= 1.9.0~~),\n libssl-dev,\n",
        );
        match infer_libgit2_version(&repo) {
            Inference::Inferred { value, .. } => assert_eq!(value, "1.9.0~~"),
            other => panic!("expected Inferred, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn infer_libgit2_version_without_constraint() {
        let repo = temp_repo();
        write(&repo, "debian/control", "Build-Depends:\n libgit2-dev,\n");
        match infer_libgit2_version(&repo) {
            Inference::Inferred { value, .. } => assert!(value.is_empty()),
            other => panic!("expected Inferred, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn infer_libgit2_version_absent_returns_could_not_infer() {
        let repo = temp_repo();
        write(&repo, "debian/control", "Build-Depends:\n libssl-dev,\n");
        assert!(matches!(
            infer_libgit2_version(&repo),
            Inference::CouldNotInfer(_)
        ));
        let _ = fs::remove_dir_all(&repo);
    }

    // ── infer_dh_cargo ─────────────────────────────────────────────────────

    #[test]
    fn infer_dh_cargo_with_constraint() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/control",
            "Build-Depends:\n dh-cargo (>= 28ubuntu1~),\n",
        );
        match infer_dh_cargo(&repo) {
            Inference::Inferred { value, .. } => assert_eq!(value, "28ubuntu1~"),
            other => panic!("expected Inferred, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn infer_dh_cargo_absent_returns_could_not_infer() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/control",
            "Build-Depends:\n debhelper (>= 13),\n",
        );
        assert!(matches!(infer_dh_cargo(&repo), Inference::CouldNotInfer(_)));
        let _ = fs::remove_dir_all(&repo);
    }

    // ── infer_pkgconf ──────────────────────────────────────────────────────

    #[test]
    fn infer_pkgconf_present() {
        let repo = temp_repo();
        write(&repo, "debian/control", "Build-Depends:\n pkgconf,\n");
        match infer_pkgconf(&repo) {
            Inference::Inferred { value, .. } => assert_eq!(value, "present"),
            other => panic!("expected Inferred, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn infer_pkgconf_pkg_config_in_use() {
        let repo = temp_repo();
        write(&repo, "debian/control", "Build-Depends:\n pkg-config,\n");
        match infer_pkgconf(&repo) {
            Inference::Inferred { value, .. } => assert_eq!(value, "pkg-config"),
            other => panic!("expected Inferred, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn infer_pkgconf_absent_returns_could_not_infer() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/control",
            "Build-Depends:\n debhelper (>= 13),\n",
        );
        assert!(matches!(infer_pkgconf(&repo), Inference::CouldNotInfer(_)));
        let _ = fs::remove_dir_all(&repo);
    }

    // ── infer_cmake ────────────────────────────────────────────────────────

    #[test]
    fn infer_cmake_with_constraint() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/control",
            "Build-Depends:\n cmake (>= 3.0) | cmake3,\n",
        );
        match infer_cmake(&repo) {
            Inference::Inferred { value, .. } => assert_eq!(value, "3.0"),
            other => panic!("expected Inferred, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn infer_cmake_skips_cmake_mozilla() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/control",
            "Build-Depends:\n cmake-mozilla (>= 3.0),\n",
        );
        assert!(matches!(infer_cmake(&repo), Inference::CouldNotInfer(_)));
        let _ = fs::remove_dir_all(&repo);
    }

    // ── infer_debhelper_compat ─────────────────────────────────────────────

    #[test]
    fn infer_debhelper_compat_equals() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/control",
            "Build-Depends:\n debhelper-compat (= 13),\n",
        );
        match infer_debhelper_compat(&repo) {
            Inference::Inferred { value, .. } => assert_eq!(value, "13"),
            other => panic!("expected Inferred, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn infer_debhelper_compat_absent_returns_could_not_infer() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/control",
            "Build-Depends:\n debhelper (>= 13),\n",
        );
        assert!(matches!(
            infer_debhelper_compat(&repo),
            Inference::CouldNotInfer(_)
        ));
        let _ = fs::remove_dir_all(&repo);
    }

    // ── word-boundary helpers ──────────────────────────────────────────────

    #[test]
    fn contains_word_matches_bare_word() {
        assert!(contains_word(" pkgconf,", "pkgconf"));
        assert!(contains_word("pkgconf", "pkgconf"));
        assert!(contains_word(" pkgconf ", "pkgconf"));
    }

    #[test]
    fn contains_word_does_not_match_substring() {
        // `pkg-config` should not match `pkgconf` because `-` is not a word
        // boundary.
        assert!(!contains_word("pkg-config", "pkgconf"));
        assert!(!contains_word("cmake-mozilla", "cmake"));
    }

    #[test]
    fn find_word_returns_index() {
        assert_eq!(find_word(" pkgconf,", "pkgconf"), Some(1));
        assert_eq!(find_word("pkg-config", "pkgconf"), None);
    }

    // ── CheckResult::is_ok ─────────────────────────────────────────────────

    #[test]
    fn check_result_is_ok_only_when_available() {
        let ok = CheckResult {
            name: "test",
            inference: Inference::Inferred {
                value: "x".to_owned(),
                source: "src".to_owned(),
            },
            archive_status: ArchiveStatus::Available("1.0".to_owned()),
            guidance: "",
            url: "u".to_owned(),
        };
        assert!(ok.is_ok());

        let not_ok = CheckResult {
            name: "test",
            inference: Inference::Inferred {
                value: "x".to_owned(),
                source: "src".to_owned(),
            },
            archive_status: ArchiveStatus::NotPublished,
            guidance: "",
            url: "u".to_owned(),
        };
        assert!(!not_ok.is_ok());
    }
}
