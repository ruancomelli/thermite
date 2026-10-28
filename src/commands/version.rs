//! The `thermite version` command group.
//!
//! Provides subcommands for parsing, explaining, formatting, and bumping
//! Ubuntu `rustc` package version strings.

use std::path::Path;

use crate::error::{Result, ThermiteError};
use crate::steps::changelog::read_current_version;
use crate::types::package_version::RustcPackageVersion;
use crate::types::versions::RustVersion;

// ---------------------------------------------------------------------------
// Public API (called from bin/thermite.rs)
// ---------------------------------------------------------------------------

/// Run the `version parse` subcommand.
pub fn run_parse(input: Option<&str>, json: bool) -> Result<()> {
    let version_str = resolve_version_input(input)?;
    let v = RustcPackageVersion::parse(&version_str)?;

    if json {
        let output =
            serde_json::to_string(&v).map_err(|e| ThermiteError::Io(std::io::Error::other(e)))?;
        println!("{output}");
    } else {
        print_parsed(&v);
    }

    Ok(())
}

/// Run the `version explain` subcommand.
pub fn run_explain(input: Option<&str>) -> Result<()> {
    let version_str = resolve_version_input(input)?;
    let v = RustcPackageVersion::parse(&version_str)?;
    print_explanation(&v, &version_str);
    Ok(())
}

/// Parameters for the `thermite version format` subcommand.
///
/// Collected into a struct to keep `run_format` below clippy's argument-count
/// threshold and to mirror the `BackportParams` / `UpdateParams` convention.
#[derive(Debug)]
pub struct FormatParams<'a> {
    /// Full upstream Rust version, e.g. `"1.95.0"`.
    pub upstream: &'a str,
    /// Optional `+dfsgN` repack number.
    pub repack_number: Option<u32>,
    /// Explicit Ubuntu series (e.g. `"24.04"`), as an alternative to `release`.
    pub series: Option<&'a str>,
    /// Ubuntu release adjective (e.g. `"noble"`), resolved to a series.
    pub release: Option<&'a str>,
    /// Repack number for the backport portion of the version.
    pub backport_repack: Option<u32>,
    /// Whether this is a stage0 (bootstrap) build.
    pub stage0: bool,
    /// Ubuntu revision after `0ubuntu`.
    pub ubuntu_revision: u32,
    /// Backport revision after the series suffix; defaults to `1` when a
    /// series is present.
    pub backport_revision: Option<u32>,
    /// PPA number for a `~ppaN` suffix.
    pub ppa: Option<u32>,
}

/// Run the `version format` subcommand.
pub fn run_format(params: &FormatParams<'_>) -> Result<()> {
    let upstream_ver = RustVersion::parse(params.upstream)?;

    // Resolve the series: either directly provided or via release adjective.
    let resolved_series = resolve_series(params.series, params.release)?;

    // Validate: backport-specific options require a series.
    if resolved_series.is_none() {
        if params.backport_repack.is_some() {
            return Err(ThermiteError::InvalidRustVersion(
                "--backport-repack requires --series or --release".to_owned(),
            ));
        }
        if params.stage0 {
            return Err(ThermiteError::InvalidRustVersion(
                "--stage0 requires --series or --release".to_owned(),
            ));
        }
        if params.backport_revision.is_some() {
            return Err(ThermiteError::InvalidRustVersion(
                "--backport-revision requires --series or --release".to_owned(),
            ));
        }
    }

    // Default backport_revision to 1 when series is present.
    let bp_rev = if resolved_series.is_some() {
        Some(params.backport_revision.unwrap_or(1))
    } else {
        None
    };

    let mut v = RustcPackageVersion::new(
        upstream_ver,
        params.repack_number,
        resolved_series,
        params.backport_repack,
        params.stage0,
        params.ubuntu_revision,
        bp_rev,
    );
    v.set_ppa(params.ppa);

    println!("{v}");
    Ok(())
}

/// Run the `version bump` subcommand.
pub fn run_bump(input: Option<&str>, operation: &BumpOperation, json: bool) -> Result<()> {
    let version_str = resolve_version_input(input)?;
    let mut v = RustcPackageVersion::parse(&version_str)?;

    apply_bump_operation(&mut v, operation)?;

    if json {
        let output =
            serde_json::to_string(&v).map_err(|e| ThermiteError::Io(std::io::Error::other(e)))?;
        println!("{output}");
    } else {
        println!("{v}");
    }

    Ok(())
}

/// Resolve a version string from the given input.
///
/// The `input` may be:
/// - `None` — use the current working directory
/// - A version string (e.g. `"1.85.0+dfsg-0ubuntu1"`) — returned as-is
/// - A path to a `debian/changelog` file — version read from the file
/// - A path to a directory — version read from `<dir>/debian/changelog`
///
/// Detection order:
/// 1. If the input string corresponds to an existing filesystem path, treat
///    it as a changelog file (if it's a regular file) or as a package root
///    directory (if it's a directory).
/// 2. Otherwise treat the string as a literal version string.
/// 3. If no input is given, default to the current directory.
pub fn resolve_version_input(input: Option<&str>) -> Result<String> {
    let s = input.unwrap_or(".");
    let path = Path::new(s);

    if path.exists() {
        if path.is_dir() {
            let changelog = path.join("debian/changelog");
            read_current_version(&changelog)
        } else {
            // Assume it's a changelog file.
            read_current_version(path)
        }
    } else {
        // Not an existing path — treat as a literal version string.
        Ok(s.to_owned())
    }
}

// ---------------------------------------------------------------------------
// Bump operations (re-exported from types/package_version.rs)
// ---------------------------------------------------------------------------

pub use crate::types::package_version::BumpOperation;
use crate::types::package_version::{apply_bump_operation, resolve_series};

/// Print the parsed version in human-readable key-value format.
fn print_parsed(v: &RustcPackageVersion) {
    println!("Upstream version:  {}", v.upstream);
    match v.repack_number {
        Some(n) => println!("Repack number:     {n}"),
        None => println!("Repack number:     (none)"),
    }
    match &v.series {
        Some(s) => println!("Series:            {s}"),
        None => println!("Series:            (none)"),
    }
    match v.backport_repack {
        Some(n) => println!("Backport repack:   {n}"),
        None => println!("Backport repack:   (none)"),
    }
    println!("Stage0:            {}", if v.stage0 { "yes" } else { "no" });
    println!("Ubuntu revision:   {}", v.ubuntu_revision);
    match v.backport_revision {
        Some(n) => println!("Backport revision: {n}"),
        None => println!("Backport revision: (none)"),
    }
    match v.ppa {
        Some(n) => println!("PPA:               {n}"),
        None => println!("PPA:               (none)"),
    }
}

/// Print a detailed explanation of each component of the version string.
fn print_explanation(v: &RustcPackageVersion, original: &str) {
    println!();
    println!("  {original}");
    println!();

    // Upstream version
    println!("  Upstream version: {}", v.upstream);
    println!("    The upstream Rust toolchain version from the Rust Foundation.");
    println!("    Only changes when upstream releases a patch (e.g. 1.85.0 -> 1.85.1).");
    println!();

    // +dfsg repack
    match v.repack_number {
        None => {
            println!("  Repack: +dfsg");
            println!("    The orig tarball has not been modified since the initial upload.");
            println!("    '+dfsg' is always present because unneeded dependencies are pruned.");
        }
        Some(n) => {
            println!("  Repack: +dfsg{n}");
            println!(
                "    The orig tarball has been repacked {n} time(s) after the initial upload."
            );
            println!("    Each repack resets the Ubuntu revision back to 1.");
        }
    }
    println!();

    // Series (backport)
    if let Some(series) = &v.series {
        println!("  Backport series: ~{series}");
        println!("    This is a backport targeting Ubuntu {series}.");
        println!("    The series number is added regardless of whether the orig tarball");
        println!("    was modified for the backport.");
        println!();

        // Backport repack
        if let Some(bp_repack) = v.backport_repack {
            println!("  Backport repack: .{bp_repack}");
            println!("    The orig tarball was modified {bp_repack} additional time(s) for this");
            println!("    specific series after the initial backport upload.");
            println!();
        }
    }

    // Stage0
    if v.stage0 {
        println!("  Stage0 bootstrap: ~stage0");
        println!("    This is a stage0 bootstrap package used to start the bootstrapping");
        println!("    process. It should never be uploaded to the Archive (PPA only).");
        println!();
    }

    // Ubuntu revision
    println!("  Ubuntu revision: -0ubuntu{}", v.ubuntu_revision);
    if v.ubuntu_revision == 1 {
        println!("    Initial upload for the given upstream source.");
    } else {
        println!(
            "    The {} Ubuntu revision for this upstream source.",
            ordinal(v.ubuntu_revision)
        );
    }
    println!("    The '0' before 'ubuntu' means this package is never synced from Debian.");
    println!();

    // Backport revision
    if let Some(bp_rev) = v.backport_revision {
        let series = v.series.as_deref().unwrap_or("??");
        println!("  Backport revision: ~{series}.{bp_rev}");
        if bp_rev == 1 {
            println!("    Initial upload of this backport.");
        } else {
            println!(
                "    The {} revision of this backport (changes that don't affect the",
                ordinal(bp_rev)
            );
            println!("    orig tarball).");
        }
        println!();
    }

    // PPA suffix
    if let Some(ppa) = v.ppa {
        println!("  PPA: ~ppa{ppa}");
        println!(
            "    This is the {} push to a Personal Package Archive for testing.",
            ordinal(ppa)
        );
        println!("    PPA suffixes are temporary and should not be committed to version");
        println!("    control or uploaded to the main Archive.");
        println!();
    }
}

/// Return the English ordinal suffix for a number (e.g. "1st", "2nd", "3rd").
fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (1, 11) => "th",
        (2, 12) => "th",
        (3, 13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_series_from_release() {
        let result = resolve_series(None, Some("noble")).unwrap();
        assert_eq!(result, Some("24.04".to_owned()));
    }

    #[test]
    fn resolve_series_direct() {
        let result = resolve_series(Some("22.04"), None).unwrap();
        assert_eq!(result, Some("22.04".to_owned()));
    }

    #[test]
    fn resolve_series_both_errors() {
        let result = resolve_series(Some("22.04"), Some("noble"));
        assert!(result.is_err());
    }

    #[test]
    fn resolve_series_neither() {
        let result = resolve_series(None, None).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn resolve_series_unknown_release() {
        let result = resolve_series(None, Some("nonexistent"));
        assert!(result.is_err());
    }

    #[test]
    fn ordinal_formatting() {
        assert_eq!(ordinal(1), "1st");
        assert_eq!(ordinal(2), "2nd");
        assert_eq!(ordinal(3), "3rd");
        assert_eq!(ordinal(4), "4th");
        assert_eq!(ordinal(11), "11th");
        assert_eq!(ordinal(12), "12th");
        assert_eq!(ordinal(13), "13th");
        assert_eq!(ordinal(21), "21st");
        assert_eq!(ordinal(22), "22nd");
        assert_eq!(ordinal(23), "23rd");
    }

    #[test]
    fn run_parse_non_backport() {
        // Just ensure it doesn't panic/error.
        run_parse(Some("1.85.0+dfsg-0ubuntu1"), false).unwrap();
    }

    #[test]
    fn run_parse_json() {
        // Ensure JSON output doesn't panic.
        run_parse(Some("1.85.0+dfsg3-0ubuntu5"), true).unwrap();
    }

    #[test]
    fn run_parse_backport() {
        run_parse(Some("1.90.0+dfsg2~24.04-0ubuntu3~24.04.1"), false).unwrap();
    }

    #[test]
    fn run_explain_works() {
        run_explain(Some("1.90.0+dfsg2~24.04-0ubuntu3~24.04.1")).unwrap();
    }

    #[test]
    fn run_explain_stage0() {
        run_explain(Some("1.92.0+dfsg~24.04~stage0-0ubuntu1~24.04.3")).unwrap();
    }

    #[test]
    fn run_format_simple() {
        run_format(&FormatParams {
            upstream: "1.95.0",
            repack_number: None,
            series: None,
            release: None,
            backport_repack: None,
            stage0: false,
            ubuntu_revision: 1,
            backport_revision: None,
            ppa: None,
        })
        .unwrap();
    }

    #[test]
    fn run_format_with_release() {
        run_format(&FormatParams {
            upstream: "1.95.0",
            repack_number: None,
            series: None,
            release: Some("noble"),
            backport_repack: None,
            stage0: false,
            ubuntu_revision: 1,
            backport_revision: None,
            ppa: None,
        })
        .unwrap();
    }

    #[test]
    fn run_format_with_series() {
        run_format(&FormatParams {
            upstream: "1.95.0",
            repack_number: Some(2),
            series: Some("24.04"),
            release: None,
            backport_repack: None,
            stage0: false,
            ubuntu_revision: 3,
            backport_revision: Some(1),
            ppa: None,
        })
        .unwrap();
    }

    #[test]
    fn run_format_backport_repack_requires_series() {
        let result = run_format(&FormatParams {
            upstream: "1.95.0",
            repack_number: None,
            series: None,
            release: None,
            backport_repack: Some(1),
            stage0: false,
            ubuntu_revision: 1,
            backport_revision: None,
            ppa: None,
        });
        assert!(result.is_err());
    }

    #[test]
    fn run_format_stage0_requires_series() {
        let result = run_format(&FormatParams {
            upstream: "1.95.0",
            repack_number: None,
            series: None,
            release: None,
            backport_repack: None,
            stage0: true,
            ubuntu_revision: 1,
            backport_revision: None,
            ppa: None,
        });
        assert!(result.is_err());
    }

    #[test]
    fn run_bump_ubuntu_revision() {
        run_bump(
            Some("1.95.0+dfsg-0ubuntu1"),
            &BumpOperation::UbuntuRevision,
            false,
        )
        .unwrap();
    }

    #[test]
    fn run_bump_repack() {
        run_bump(Some("1.95.0+dfsg-0ubuntu3"), &BumpOperation::Repack, false).unwrap();
    }

    #[test]
    fn run_bump_patch_release() {
        run_bump(
            Some("1.95.0+dfsg3-0ubuntu5"),
            &BumpOperation::PatchRelease {
                upstream: "1.95.1".to_owned(),
            },
            false,
        )
        .unwrap();
    }

    #[test]
    fn run_bump_backport() {
        run_bump(
            Some("1.95.0+dfsg-0ubuntu2"),
            &BumpOperation::Backport {
                series: None,
                release: Some("noble".to_owned()),
            },
            false,
        )
        .unwrap();
    }

    #[test]
    fn run_bump_backport_revision() {
        run_bump(
            Some("1.90.0+dfsg2~24.04-0ubuntu3~24.04.1"),
            &BumpOperation::BackportRevision,
            false,
        )
        .unwrap();
    }

    #[test]
    fn run_bump_backport_repack() {
        run_bump(
            Some("1.90.0+dfsg2~24.04-0ubuntu3~24.04.2"),
            &BumpOperation::BackportRepack,
            false,
        )
        .unwrap();
    }

    #[test]
    fn run_bump_retarget() {
        run_bump(
            Some("1.90.0+dfsg2~24.04-0ubuntu3~24.04.1"),
            &BumpOperation::Retarget {
                series: Some("22.04".to_owned()),
                release: None,
            },
            false,
        )
        .unwrap();
    }

    #[test]
    fn run_bump_backport_missing_series_errors() {
        let result = run_bump(
            Some("1.95.0+dfsg-0ubuntu2"),
            &BumpOperation::Backport {
                series: None,
                release: None,
            },
            false,
        );
        assert!(result.is_err());
    }

    #[test]
    fn run_bump_json_output() {
        run_bump(
            Some("1.95.0+dfsg-0ubuntu1"),
            &BumpOperation::UbuntuRevision,
            true,
        )
        .unwrap();
    }

    #[test]
    fn resolve_version_input_missing_path_is_literal() {
        // A non-existent path string is treated as a literal version string.
        let result = resolve_version_input(Some("/nonexistent/debian/changelog")).unwrap();
        assert_eq!(result, "/nonexistent/debian/changelog");
    }
}
