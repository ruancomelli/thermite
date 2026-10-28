use std::path::{Path, PathBuf};

use tracing::info;

use crate::error::{Result, ThermiteError};
use crate::steps::{overlay, tarball_fetch, uscan, vendor};
use crate::types::params::{TarballAction, TarballParams};
use crate::ui::{print_info_box, print_phase_header, prompt_select};

/// Which tarball(s) the invocation targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TarballTarget {
    /// `.orig.tar.xz` only.
    Orig,
    /// `.orig-vendor.tar.xz` only.
    Vendor,
    /// Both, orig first.
    All,
}

impl TarballTarget {
    /// Whether the vendor tarball is in scope for this invocation.
    fn wants_vendor(self) -> bool {
        matches!(self, TarballTarget::Vendor | TarballTarget::All)
    }

    /// Whether the orig tarball is in scope for this invocation.
    fn wants_orig(self) -> bool {
        matches!(self, TarballTarget::Orig | TarballTarget::All)
    }
}

/// Decide what to do when `generate` finds the tarball already present.
///
/// Without `--force` the command refuses with a message pointing at the
/// existing file; with `--force` the existing file is removed and the caller
/// may proceed with regeneration.
async fn handle_existing_for_generate(tarball: &Path, force: bool) -> Result<()> {
    if !tarball.exists() {
        return Ok(());
    }
    if force {
        info!("removing existing tarball {}", tarball.display());
        std::fs::remove_file(tarball).map_err(ThermiteError::Io)?;
        return Ok(());
    }
    Err(ThermiteError::CommandFailed {
        cmd: "tarball generate".to_owned(),
        code: 0,
        stdout: String::new(),
        stderr: format!(
            "tarball already exists: {}\n\
             Re-run with --force to overwrite it, or use 'thermite tarball download' \
             to fetch it only when missing.",
            tarball.display(),
        ),
    })
}

/// Build the expected path of the `kind` tarball for `params` in `parent_dir`.
fn expected_tarball_path(
    params: &TarballParams,
    parent_dir: &Path,
    kind: tarball_fetch::TarballKind,
) -> PathBuf {
    let rust_ver = &params.rust_version;
    let name = tarball_fetch::expected_tarball_name(
        &rust_ver.short(),
        rust_ver,
        &params.dfsg_suffix(),
        kind,
    );
    parent_dir.join(name)
}

/// Ensure the orig tarball exists locally, either by reusing an existing file
/// or by fetching it from the staging PPA / Ubuntu archive.
///
/// Falls back to a manual-placement prompt when no candidate source has it.
async fn download_orig(params: &TarballParams, parent_dir: &Path) -> Result<PathBuf> {
    let rust_ver = &params.rust_version;
    let rust_short = rust_ver.short();
    let suffix = params.dfsg_suffix();
    let expected = expected_tarball_path(params, parent_dir, tarball_fetch::TarballKind::Orig);

    if expected.exists() {
        println!("  Orig tarball already present: {}", expected.display());
        return Ok(expected);
    }

    print_info_box(
        "Orig tarball download",
        &[
            "Fetching the orig tarball automatically from, in order:",
            "  1. the rust-toolchain staging PPA (previous backport uploads), and",
            "  2. the primary Ubuntu archive (candidates resolved via rmadison).",
            "",
            "If neither source has it, you will be asked to place the file manually at:",
            &format!("  {}", expected.display()),
        ],
    );

    let fetched = tarball_fetch::fetch_tarball(
        &rust_short,
        rust_ver,
        &suffix,
        parent_dir,
        tarball_fetch::TarballKind::Orig,
    )
    .await?;
    if fetched.is_none() {
        println!("  Automated download unavailable — place the tarball manually.");
        if prompt_select(
            "Place the tarball at the path shown above, then continue.",
            &["I've placed the tarball — continue", "Abort"],
            0,
        ) != 0
        {
            return Err(ThermiteError::CommandFailed {
                cmd: "download orig tarball".to_owned(),
                code: 0,
                stdout: String::new(),
                stderr: "aborted at manual-placement prompt".to_owned(),
            });
        }
    }
    if !expected.exists() {
        return Err(ThermiteError::CommandFailed {
            cmd: "download orig tarball".to_owned(),
            code: 0,
            stdout: String::new(),
            stderr: format!(
                "tarball not found: {}\n\
                 Place the tarball at that exact path, then re-run.",
                expected.display(),
            ),
        });
    }
    Ok(expected)
}

/// Ensure the vendor tarball exists locally, either by reusing an existing
/// file or by fetching it from the staging PPA / Ubuntu archive.
///
/// Falls back to a manual-placement prompt when no candidate source has it.
async fn download_vendor(params: &TarballParams, parent_dir: &Path) -> Result<PathBuf> {
    let rust_ver = &params.rust_version;
    let rust_short = rust_ver.short();
    let suffix = params.dfsg_suffix();
    let expected =
        expected_tarball_path(params, parent_dir, tarball_fetch::TarballKind::OrigVendor);

    if expected.exists() {
        println!("  Vendor tarball already present: {}", expected.display());
        return Ok(expected);
    }

    print_info_box(
        "Vendor tarball download",
        &[
            "Fetching the vendor tarball automatically from, in order:",
            "  1. the rust-toolchain staging PPA (previous backport uploads), and",
            "  2. the primary Ubuntu archive (candidates resolved via rmadison).",
            "",
            "If neither source has it, you will be asked to place the file manually at:",
            &format!("  {}", expected.display()),
        ],
    );

    let fetched = tarball_fetch::fetch_tarball(
        &rust_short,
        rust_ver,
        &suffix,
        parent_dir,
        tarball_fetch::TarballKind::OrigVendor,
    )
    .await?;
    if fetched.is_none() {
        println!("  Automated download unavailable — place the vendor tarball manually.");
        if prompt_select(
            "Place the vendor tarball at the path shown above, then continue.",
            &["I've placed the vendor tarball — continue", "Abort"],
            0,
        ) != 0
        {
            return Err(ThermiteError::CommandFailed {
                cmd: "download vendor tarball".to_owned(),
                code: 0,
                stdout: String::new(),
                stderr: "aborted at manual-placement prompt".to_owned(),
            });
        }
    }
    if !expected.exists() {
        return Err(ThermiteError::CommandFailed {
            cmd: "download vendor tarball".to_owned(),
            code: 0,
            stdout: String::new(),
            stderr: format!(
                "vendor tarball not found: {}\n\
                 Place the tarball at that exact path, then re-run.",
                expected.display(),
            ),
        });
    }
    Ok(expected)
}

/// Regenerate the orig tarball by running uscan.
async fn generate_orig(
    params: &TarballParams,
    repo_dir: &Path,
    parent_dir: &Path,
) -> Result<PathBuf> {
    let rust_ver = &params.rust_version;
    let suffix = params.dfsg_suffix();
    let expected = expected_tarball_path(params, parent_dir, tarball_fetch::TarballKind::Orig);
    handle_existing_for_generate(&expected, params.force).await?;

    let uscan_log = parent_dir.join(format!("uscan-{rust_ver}-tarball.log"));
    info!("running uscan --download-version {rust_ver}");
    uscan::run_uscan(repo_dir, rust_ver, &suffix, &uscan_log).await
}

/// Regenerate the vendor tarball: install the matching toolchain via rustup,
/// install `cargo-vendor-filterer`, then run `debian/rules vendor-tarball`.
async fn generate_vendor(
    params: &TarballParams,
    repo_dir: &Path,
    parent_dir: &Path,
) -> Result<PathBuf> {
    let rust_ver = &params.rust_version;
    let suffix = params.dfsg_suffix();
    let expected =
        expected_tarball_path(params, parent_dir, tarball_fetch::TarballKind::OrigVendor);
    handle_existing_for_generate(&expected, params.force).await?;

    info!("ensuring rustup is installed");
    vendor::ensure_rustup_installed().await?;

    info!("installing Rust toolchain {rust_ver}");
    let rust_bootstrap_dir = vendor::rustup_install_toolchain(rust_ver).await?;

    info!("installing cargo-vendor-filterer");
    vendor::install_cargo_vendor_filterer(rust_ver).await?;

    let tarball =
        vendor::generate_vendor_tarball_clean(repo_dir, &rust_bootstrap_dir, rust_ver, &suffix)
            .await?;

    Ok(tarball)
}

/// Locate an already-obtained tarball in `parent_dir`.
///
/// `overlay` never fetches or produces tarballs, so a missing file is an
/// error pointing the caller at `download` / `generate`.
fn locate_expected_tarball(
    params: &TarballParams,
    parent_dir: &Path,
    kind: tarball_fetch::TarballKind,
) -> Result<PathBuf> {
    let expected = expected_tarball_path(params, parent_dir, kind);

    if !expected.exists() {
        return Err(ThermiteError::CommandFailed {
            cmd: "tarball overlay".to_owned(),
            code: 0,
            stdout: String::new(),
            stderr: format!(
                "tarball not found: {}\n\
                 Obtain it first with 'thermite tarball download' or \
                 'thermite tarball generate'.",
                expected.display(),
            ),
        });
    }
    Ok(expected)
}

/// Overlay an existing orig tarball into `repo_dir`, stripping the archive's
/// top-level source directory so its contents land in the repo root.
async fn overlay_orig(
    params: &TarballParams,
    repo_dir: &Path,
    parent_dir: &Path,
) -> Result<PathBuf> {
    let tarball = locate_expected_tarball(params, parent_dir, tarball_fetch::TarballKind::Orig)?;
    println!("  Overlaying orig tarball into {} …", repo_dir.display());
    overlay::overlay_orig_tarball(&tarball, repo_dir).await?;
    Ok(tarball)
}

/// Overlay an existing vendor tarball into `repo_dir`, honouring
/// `params.overlay_replace` (merge or clean replace of `vendor/`).
async fn overlay_vendor(
    params: &TarballParams,
    repo_dir: &Path,
    parent_dir: &Path,
) -> Result<PathBuf> {
    let tarball =
        locate_expected_tarball(params, parent_dir, tarball_fetch::TarballKind::OrigVendor)?;
    let mode = if params.overlay_replace {
        "clean replace"
    } else {
        "merge"
    };
    println!(
        "  Overlaying vendor/ into {} ({mode}) …",
        repo_dir.display()
    );
    overlay::overlay_vendor_dir(&tarball, repo_dir, params.overlay_replace).await?;
    Ok(tarball)
}

/// Run the `thermite tarball` workflow for `target` with the selected action.
pub async fn run(params: &TarballParams, repo_dir: &Path, target: TarballTarget) -> Result<()> {
    if !repo_dir.join("debian/changelog").exists() {
        return Err(ThermiteError::NotADebianPackageRoot(
            repo_dir.display().to_string(),
        ));
    }

    let parent_dir = repo_dir.parent().unwrap_or(repo_dir).to_path_buf();
    let series_display = params
        .series
        .as_ref()
        .map(|r| r.as_str().to_owned())
        .unwrap_or_else(|| "(none — plain +dfsg naming)".to_owned());

    let mut lines = vec![
        format!("  Rust version : {}", params.rust_version),
        format!("  Series       : {series_display}"),
        format!("  Repo dir     : {}", repo_dir.display()),
        format!("  Parent dir   : {}", parent_dir.display()),
    ];
    if params.action == TarballAction::Generate {
        lines.push(format!("  Force        : {}", params.force));
    }
    if params.action == TarballAction::Overlay {
        lines.push(format!(
            "  Overlay mode : {}",
            if params.overlay_replace {
                "clean replace"
            } else {
                "merge"
            }
        ));
    }
    let line_refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    print_info_box("Tarball parameters", &line_refs);

    if target.wants_orig() {
        print_phase_header(0, "Orig Tarball");
        let tarball = match params.action {
            TarballAction::Download => download_orig(params, &parent_dir).await?,
            TarballAction::Generate => generate_orig(params, repo_dir, &parent_dir).await?,
            TarballAction::Overlay => overlay_orig(params, repo_dir, &parent_dir).await?,
        };
        println!("  Orig tarball: {}", tarball.display());
    }

    if target.wants_vendor() {
        print_phase_header(1, "Vendor Tarball");
        let tarball = match params.action {
            TarballAction::Download => download_vendor(params, &parent_dir).await?,
            TarballAction::Generate => generate_vendor(params, repo_dir, &parent_dir).await?,
            TarballAction::Overlay => overlay_vendor(params, repo_dir, &parent_dir).await?,
        };
        println!("  Vendor tarball: {}", tarball.display());
    }

    println!("\nthermite tarball complete.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "thermite-tarball-{label}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn target_scope_flags_are_consistent() {
        assert!(TarballTarget::Orig.wants_orig());
        assert!(!TarballTarget::Orig.wants_vendor());
        assert!(!TarballTarget::Vendor.wants_orig());
        assert!(TarballTarget::Vendor.wants_vendor());
        assert!(TarballTarget::All.wants_orig());
        assert!(TarballTarget::All.wants_vendor());
    }

    #[tokio::test]
    async fn generate_refuses_existing_tarball_without_force() {
        let tmp = temp_dir("refuse");
        let tarball = tmp.join("rustc-1.85_1.85.0+dfsg.orig.tar.xz");
        fs::write(&tarball, b"existing").unwrap();

        let result = handle_existing_for_generate(&tarball, false).await;
        assert!(result.is_err(), "expected refusal when tarball exists");
        assert!(tarball.exists(), "refusal must not delete the tarball");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn generate_with_force_removes_existing_tarball() {
        let tmp = temp_dir("force");
        let tarball = tmp.join("rustc-1.85_1.85.0+dfsg.orig.tar.xz");
        fs::write(&tarball, b"existing").unwrap();

        handle_existing_for_generate(&tarball, true)
            .await
            .expect("force should remove the existing tarball");
        assert!(!tarball.exists(), "tarball should have been removed");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn generate_is_noop_when_tarball_missing() {
        let tmp = temp_dir("missing");
        let tarball = tmp.join("rustc-1.85_1.85.0+dfsg.orig.tar.xz");

        handle_existing_for_generate(&tarball, false)
            .await
            .expect("missing tarball should be a no-op");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn overlay_locator_errors_when_tarball_missing() {
        let tmp = temp_dir("overlay-missing");
        let params =
            TarballParams::new(TarballAction::Overlay, "1.85.0", None, false, false).unwrap();

        let err =
            locate_expected_tarball(&params, &tmp, tarball_fetch::TarballKind::Orig).unwrap_err();
        let ThermiteError::CommandFailed { stderr, .. } = &err else {
            panic!("expected CommandFailed, got: {err:?}")
        };
        assert!(
            stderr.contains("rustc-1.85_1.85.0+dfsg.orig.tar.xz"),
            "error must name the expected tarball: {stderr}"
        );
        assert!(
            stderr.contains("thermite tarball download"),
            "error must point at download/generate: {stderr}"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn overlay_locator_finds_existing_tarball() {
        let tmp = temp_dir("overlay-present");
        let tarball = tmp.join("rustc-1.85_1.85.0+dfsg.orig.tar.xz");
        fs::write(&tarball, b"tarball").unwrap();
        let params =
            TarballParams::new(TarballAction::Overlay, "1.85.0", None, false, false).unwrap();

        let found = locate_expected_tarball(&params, &tmp, tarball_fetch::TarballKind::Orig)
            .expect("existing tarball should be located");
        assert_eq!(found, tarball);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn overlay_locator_honours_series_suffix() {
        let tmp = temp_dir("overlay-series");
        let tarball = tmp.join("rustc-1.85_1.85.0+dfsg~24.04.orig-vendor.tar.xz");
        fs::write(&tarball, b"tarball").unwrap();
        let params = TarballParams::new(
            TarballAction::Overlay,
            "1.85.0",
            Some("noble"),
            false,
            false,
        )
        .unwrap();

        let found = locate_expected_tarball(&params, &tmp, tarball_fetch::TarballKind::OrigVendor)
            .expect("series-suffixed tarball should be located");
        assert_eq!(found, tarball);

        let _ = fs::remove_dir_all(&tmp);
    }
}
