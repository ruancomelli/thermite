use std::path::{Path, PathBuf};

use tracing::info;

use crate::error::{Result, ThermiteError};
use crate::shell::{run_command, which};
use crate::types::versions::RustVersion;

/// Ensure `rustup` is installed (available on PATH).
///
/// Returns an error with installation guidance if it is not found.
pub async fn ensure_rustup_installed() -> Result<String> {
    which("rustup").map_err(|_| {
        ThermiteError::CommandNotFound(
            "rustup (install it with: snap install rustup --classic)".to_owned(),
        )
    })
}

/// Install the Rust toolchain matching `version` via `rustup install`.
///
/// Returns the path to the installed toolchain directory
/// (`~/.rustup/toolchains/<version>-x86_64-unknown-linux-gnu`).
pub async fn rustup_install_toolchain(version: &RustVersion) -> Result<PathBuf> {
    let version_str = version.to_string();
    run_command("rustup", &["install", &version_str], Path::new("."), &[]).await?;

    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_owned());
    let toolchain_dir = PathBuf::from(format!(
        "{home}/.rustup/toolchains/{version}-x86_64-unknown-linux-gnu"
    ));
    Ok(toolchain_dir)
}

/// Install `cargo-vendor-filterer` using the Rust toolchain for `version`.
pub async fn install_cargo_vendor_filterer(version: &RustVersion) -> Result<()> {
    let version_str = version.to_string();
    let toolchain_flag = format!("+{version_str}");
    run_command(
        "cargo",
        &[&toolchain_flag, "install", "cargo-vendor-filterer"],
        Path::new("."),
        &[],
    )
    .await?;
    Ok(())
}

/// Generate the pruned vendor tarball component by running
/// `debian/rules vendor-tarball` with `RUST_BOOTSTRAP_DIR` set.
///
/// `dfsg_suffix` is appended after `+dfsg` in the tarball filename to match
/// the upstream version in `debian/changelog`.  For backports this is the
/// series suffix (e.g. `"~20.04"`); for main-series builds pass `""`.
///
/// Returns the path to the generated vendor tarball component.
///
/// Finding 6: use the known naming convention derived from `version` instead
/// of selecting the first matching file, making the choice deterministic even
/// when stale tarballs from prior builds are present.
pub async fn generate_vendor_tarball(
    repo_dir: &Path,
    rust_bootstrap_dir: &Path,
    version: &RustVersion,
    dfsg_suffix: &str,
) -> Result<PathBuf> {
    let parent = repo_dir.parent().unwrap_or(repo_dir);
    for temp in ["vendor", "std-vendor"] {
        let dir = parent.join(temp);
        if dir.is_dir() {
            std::fs::remove_dir_all(&dir).map_err(crate::error::ThermiteError::Io)?;
        }
    }

    let bootstrap_str = rust_bootstrap_dir.to_string_lossy().to_string();
    run_command(
        "debian/rules",
        &["vendor-tarball"],
        repo_dir,
        &[("RUST_BOOTSTRAP_DIR", &bootstrap_str)],
    )
    .await?;

    // Finding 6: construct the expected filename deterministically from the
    // Rust version rather than picking the first matching .orig-vendor.tar.xz found.
    let short = version.short();
    let tarball_name = format!("rustc-{short}_{version}+dfsg{dfsg_suffix}.orig-vendor.tar.xz");
    let tarball_path = parent.join(&tarball_name);

    if !tarball_path.exists() {
        return Err(ThermiteError::CommandFailed {
            cmd: "debian/rules vendor-tarball".to_owned(),
            code: 0,
            stdout: String::new(),
            stderr: format!(
                "expected vendor tarball not found: {}",
                tarball_path.display()
            ),
        });
    }

    Ok(tarball_path)
}

/// Returns `true` when `name` is an orig-vendor tarball for `version` carrying
/// a series suffix other than the expected one (i.e. a leftover from a
/// previous series build that would make `debian/rules vendor-tarball-quick-check`
/// abort).
fn is_stale_series_vendor_tarball(name: &str, version: &RustVersion, expected_name: &str) -> bool {
    let short = version.short();
    let prefix = format!("rustc-{short}_{version}+dfsg");
    name.starts_with(&prefix) && name.ends_with(".orig-vendor.tar.xz") && name != expected_name
}

/// Remove any stale orig-vendor tarballs for `version` in `parent_dir` that
/// would cause `debian/rules vendor-tarball-quick-check` to abort, then run
/// `debian/rules vendor-tarball` and return the generated tarball path.
///
/// Stale tarballs arise when the same package was previously built for a
/// different Ubuntu series (e.g. a `~22.04` tarball left over from a jammy
/// build when we are now targeting focal `~20.04`). Cleanup only runs in
/// series mode (non-empty `dfsg_suffix`) so plain update-style regeneration
/// never deletes series-suffixed tarballs.
pub async fn generate_vendor_tarball_clean(
    repo_dir: &Path,
    rust_bootstrap_dir: &Path,
    version: &RustVersion,
    dfsg_suffix: &str,
) -> Result<PathBuf> {
    let parent = repo_dir.parent().unwrap_or(repo_dir);

    if !dfsg_suffix.is_empty() {
        let short = version.short();
        let expected_name = format!("rustc-{short}_{version}+dfsg{dfsg_suffix}.orig-vendor.tar.xz");
        let stale: Vec<_> = std::fs::read_dir(parent)
            .map_err(ThermiteError::Io)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| is_stale_series_vendor_tarball(n, version, &expected_name))
                    .unwrap_or(false)
            })
            .collect();

        if !stale.is_empty() {
            println!("  Removing stale vendor tarballs from previous series builds:");
            for path in &stale {
                println!("    {}", path.display());
                std::fs::remove_file(path).map_err(ThermiteError::Io)?;
            }
        }
    }

    info!("generating vendor tarball");
    generate_vendor_tarball(repo_dir, rust_bootstrap_dir, version, dfsg_suffix).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::versions::RustVersion;

    /// Finding 6: the vendor tarball name is derived deterministically from the
    /// Rust version, matching the debian/rules naming convention.
    #[test]
    fn vendor_tarball_expected_name_format() {
        let version = RustVersion::parse("1.85.0").unwrap();
        let short = version.short();
        let name = format!("rustc-{short}_{version}+dfsg.orig-vendor.tar.xz");
        assert_eq!(name, "rustc-1.85_1.85.0+dfsg.orig-vendor.tar.xz");
    }

    #[test]
    fn stale_detection_flags_other_series_tarballs() {
        let version = RustVersion::parse("1.85.0").unwrap();
        let expected = "rustc-1.85_1.85.0+dfsg~20.04.orig-vendor.tar.xz";
        assert!(is_stale_series_vendor_tarball(
            "rustc-1.85_1.85.0+dfsg~22.04.orig-vendor.tar.xz",
            &version,
            expected
        ));
        assert!(is_stale_series_vendor_tarball(
            "rustc-1.85_1.85.0+dfsg1~22.04.orig-vendor.tar.xz",
            &version,
            expected
        ));
    }

    #[test]
    fn stale_detection_keeps_expected_tarball() {
        let version = RustVersion::parse("1.85.0").unwrap();
        let expected = "rustc-1.85_1.85.0+dfsg~20.04.orig-vendor.tar.xz";
        assert!(!is_stale_series_vendor_tarball(
            expected, &version, expected
        ));
    }

    #[test]
    fn stale_detection_ignores_other_kinds_and_versions() {
        let version = RustVersion::parse("1.85.0").unwrap();
        let expected = "rustc-1.85_1.85.0+dfsg~20.04.orig-vendor.tar.xz";
        assert!(!is_stale_series_vendor_tarball(
            "rustc-1.85_1.85.0+dfsg~20.04.orig.tar.xz",
            &version,
            expected
        ));
        assert!(!is_stale_series_vendor_tarball(
            "rustc-1.84_1.84.0+dfsg~22.04.orig-vendor.tar.xz",
            &version,
            expected
        ));
    }
}
