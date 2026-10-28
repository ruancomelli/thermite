use std::path::{Path, PathBuf};

use crate::error::{Result, ThermiteError};
use crate::shell::{run_command, run_interactive_command};
use crate::types::versions::{RustVersion, ShortRustVersion};

/// File base for the rust-toolchain staging PPA on Launchpad, whose published
/// source files already carry the `~<series>` suffix of a previous backport
/// upload.
const STAGING_PPA_FILES_BASE: &str =
    "https://launchpad.net/~rust-toolchain/+archive/ubuntu/staging/+files";

/// File base for the primary Ubuntu archive on Launchpad, used to reuse the
/// orig (and orig-vendor) tarball published for the same upstream version.
const PRIMARY_ARCHIVE_FILES_BASE: &str = "https://launchpad.net/ubuntu/+archive/primary/+files";

/// Which of the two backport source tarballs to fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TarballKind {
    /// `rustc-<short>_<ver>+dfsg~<series>.orig.tar.xz`
    Orig,
    /// `rustc-<short>_<ver>+dfsg~<series>.orig-vendor.tar.xz`
    OrigVendor,
}

impl TarballKind {
    fn tail(self) -> &'static str {
        match self {
            TarballKind::Orig => ".orig.tar.xz",
            TarballKind::OrigVendor => ".orig-vendor.tar.xz",
        }
    }
}

/// Build the exact tarball filename thermite expects in `dest_dir`.
///
/// `dfsg_suffix` is appended after `+dfsg`: `""` for plain update naming or
/// `"~<series>"` (e.g. `"~26.04"`) for backport naming.
pub fn expected_tarball_name(
    rust_short: &ShortRustVersion,
    rust_ver: &RustVersion,
    dfsg_suffix: &str,
    kind: TarballKind,
) -> String {
    format!(
        "rustc-{rust_short}_{rust_ver}+dfsg{dfsg_suffix}{}",
        kind.tail()
    )
}

/// Percent-encode a filename for use in a Launchpad `+files` URL path.
///
/// Only `+` needs encoding for the tarball names thermite builds; `~` is an
/// RFC 3986 unreserved character and is left as-is.
fn encode_launchpad_path(name: &str) -> String {
    name.replace('+', "%2B")
}

fn launchpad_files_url(base: &str, filename: &str) -> String {
    format!("{base}/{}", encode_launchpad_path(filename))
}

/// Extract the deduplicated upstream version strings (e.g. `1.96.1+dfsg`)
/// from `rmadison -u ubuntu rustc-<short>` output for every published source
/// whose upstream base matches `rust_ver`.
///
/// The upstream version is everything before the first `-`; only entries
/// starting with `<rust_ver>+dfsg` are considered, so packaging-only
/// versions without the dfsg suffix are skipped. Output order is preserved.
pub fn parse_rmadison_upstream_versions(stdout: &str, rust_ver: &str) -> Vec<String> {
    let prefix = format!("{rust_ver}+dfsg");
    let mut found: Vec<String> = Vec::new();
    for line in stdout.lines() {
        // Skip blank lines, warnings, and headers.
        if line.trim().is_empty() || !line.contains('|') {
            continue;
        }
        let parts: Vec<&str> = line.split('|').map(str::trim).collect();
        if parts.len() < 4 {
            continue;
        }
        let version = parts[1];
        let upstream = version.split('-').next().unwrap_or(version);
        if upstream.starts_with(&prefix) && !found.contains(&upstream.to_owned()) {
            found.push(upstream.to_owned());
        }
    }
    found
}

/// Download `url` to `dest` using `wget` (preferred) or `curl`, streaming
/// progress to the terminal.
///
/// The caller is responsible for cleaning up partial files on failure.
async fn download_to_file(url: &str, dest: &Path) -> Result<()> {
    let dest_str = dest.to_string_lossy().to_string();
    match run_interactive_command(
        "wget",
        &["--tries=3", "--connect-timeout=15", "-O", &dest_str, url],
        Path::new("."),
        &[],
    )
    .await
    {
        Ok(()) => Ok(()),
        Err(ThermiteError::CommandNotFound(_)) => {
            run_interactive_command(
                "curl",
                &[
                    "-fL",
                    "--retry",
                    "2",
                    "--connect-timeout",
                    "15",
                    "-o",
                    &dest_str,
                    url,
                ],
                Path::new("."),
                &[],
            )
            .await
        }
        Err(e) => Err(e),
    }
}

/// Attempt to fetch a source tarball automatically.
///
/// Candidate sources, in order:
///
/// 1. the rust-toolchain staging PPA (the file already carries the
///    `~<series>` suffix of a previous backport upload), and
/// 2. the primary Ubuntu archive, reusing the orig tarball published for the
///    same upstream version (located via `rmadison`).
///
/// `dfsg_suffix` is appended after `+dfsg` in the expected filename: `""` for
/// plain update naming or `"~<series>"` for backport naming.
///
/// Returns `Ok(Some(path))` when the tarball is available at the expected
/// location (already present or downloaded), and `Ok(None)` when no candidate
/// has it — the caller should fall back to manual placement.
pub async fn fetch_tarball(
    rust_short: &ShortRustVersion,
    rust_ver: &RustVersion,
    dfsg_suffix: &str,
    dest_dir: &Path,
    kind: TarballKind,
) -> Result<Option<PathBuf>> {
    let name = expected_tarball_name(rust_short, rust_ver, dfsg_suffix, kind);
    let dest = dest_dir.join(&name);
    if dest.exists() {
        return Ok(Some(dest));
    }

    let mut candidates = vec![(
        launchpad_files_url(STAGING_PPA_FILES_BASE, &name),
        format!("staging PPA ({name})"),
    )];
    let rmadison_query = format!("rustc-{rust_short}");
    match run_command(
        "rmadison",
        &["-u", "ubuntu", &rmadison_query],
        Path::new("."),
        &[],
    )
    .await
    {
        Ok(output) => {
            for upstream in parse_rmadison_upstream_versions(&output.stdout, &rust_ver.to_string())
            {
                let archive_name = format!("rustc-{rust_short}_{upstream}{}", kind.tail());
                candidates.push((
                    launchpad_files_url(PRIMARY_ARCHIVE_FILES_BASE, &archive_name),
                    format!("Ubuntu archive ({archive_name})"),
                ));
            }
        }
        Err(_) => println!("  rmadison failed — skipping Ubuntu archive candidates"),
    }

    for (url, label) in &candidates {
        println!("  Trying {label} …");
        match download_to_file(url, &dest).await {
            Ok(()) => {
                if std::fs::metadata(&dest).is_ok_and(|m| m.len() > 0) {
                    return Ok(Some(dest));
                }
                let _ = std::fs::remove_file(&dest);
            }
            Err(ThermiteError::CommandNotFound(_)) => {
                println!("  Neither wget nor curl found — cannot download automatically.");
                break;
            }
            Err(_) => {
                let _ = std::fs::remove_file(&dest);
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ver(patch: u32) -> RustVersion {
        RustVersion::parse(&format!("1.96.{patch}")).unwrap()
    }

    #[test]
    fn expected_names_match_thermite_contract() {
        let rust_ver = ver(0);
        let rust_short = rust_ver.short();
        assert_eq!(
            expected_tarball_name(&rust_short, &rust_ver, "~26.04", TarballKind::Orig),
            "rustc-1.96_1.96.0+dfsg~26.04.orig.tar.xz"
        );
        assert_eq!(
            expected_tarball_name(&rust_short, &rust_ver, "~26.04", TarballKind::OrigVendor),
            "rustc-1.96_1.96.0+dfsg~26.04.orig-vendor.tar.xz"
        );
        assert_eq!(
            expected_tarball_name(&rust_short, &rust_ver, "", TarballKind::Orig),
            "rustc-1.96_1.96.0+dfsg.orig.tar.xz"
        );
        assert_eq!(
            expected_tarball_name(&rust_short, &rust_ver, "", TarballKind::OrigVendor),
            "rustc-1.96_1.96.0+dfsg.orig-vendor.tar.xz"
        );
    }

    #[test]
    fn launchpad_urls_encode_plus_but_not_tilde() {
        assert_eq!(
            launchpad_files_url(
                STAGING_PPA_FILES_BASE,
                "rustc-1.96_1.96.1+dfsg~26.04.orig.tar.xz"
            ),
            "https://launchpad.net/~rust-toolchain/+archive/ubuntu/staging/+files/rustc-1.96_1.96.1%2Bdfsg~26.04.orig.tar.xz"
        );
        assert_eq!(
            launchpad_files_url(
                PRIMARY_ARCHIVE_FILES_BASE,
                "rustc-1.96_1.96.1+dfsg.orig.tar.xz"
            ),
            "https://launchpad.net/ubuntu/+archive/primary/+files/rustc-1.96_1.96.1%2Bdfsg.orig.tar.xz"
        );
    }

    #[test]
    fn parse_rmadison_finds_matching_upstream() {
        let stdout = " rustc-1.96 | 1.96.1+dfsg-0ubuntu1 | stonking/universe | source, amd64\n\
                      rustc-1.96 | 1.96.1+dfsg-0ubuntu1 | stonking | source\n";
        assert_eq!(
            parse_rmadison_upstream_versions(stdout, "1.96.1"),
            vec!["1.96.1+dfsg".to_owned()]
        );
    }

    #[test]
    fn parse_rmadison_handles_series_suffixed_versions() {
        let stdout = " rustc-1.85 | 1.85.1+dfsg0ubuntu2~bpo0-0ubuntu0.24.04.2 | noble-updates/universe | source\n\
                      rustc-1.85 | 1.85.1+dfsg0ubuntu2~bpo0-0ubuntu0.24.04.2 | noble-security/universe | source\n\
                      rustc-1.85 | 1.85.0+dfsg1~24.04-0ubuntu1~24.04.1 | noble | source\n";
        assert_eq!(
            parse_rmadison_upstream_versions(stdout, "1.85.1"),
            vec!["1.85.1+dfsg0ubuntu2~bpo0".to_owned()]
        );
        assert_eq!(
            parse_rmadison_upstream_versions(stdout, "1.85.0"),
            vec!["1.85.0+dfsg1~24.04".to_owned()]
        );
    }

    #[test]
    fn parse_rmadison_skips_non_matching_and_malformed_lines() {
        let stdout = "Get:1 https://api.launchpad.net\n\
                      \n\
                      rustc-1.96 | 1.96.1-0ubuntu1 | stonking/universe | source\n\
                      rustc-1.96 | 1.96.0+dfsg-0ubuntu1 | other | source\n\
                      rustc-1.96 | 1.96.0+dfsg1-0ubuntu1\n";
        assert_eq!(
            parse_rmadison_upstream_versions(stdout, "1.96.0"),
            vec!["1.96.0+dfsg".to_owned()]
        );
    }

    #[test]
    fn parse_rmadison_returns_empty_for_no_matches() {
        assert!(parse_rmadison_upstream_versions("", "1.96.0").is_empty());
        assert!(parse_rmadison_upstream_versions("garbage line", "1.96.0").is_empty());
    }
}
