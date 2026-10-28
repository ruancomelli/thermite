use std::path::Path;

use crate::error::{Result, ThermiteError};
use crate::shell::run_command;

/// Extract the contents of an orig tarball into `repo_dir`, stripping the
/// archive's single top-level directory (`rustc-<X.Y.Z>-src/`) so files land
/// directly in the repo root.
pub async fn overlay_orig_tarball(tarball: &Path, repo_dir: &Path) -> Result<()> {
    let tarball_str = tarball.to_string_lossy().to_string();
    let repo_str = repo_dir.to_string_lossy().to_string();
    run_command(
        "tar",
        &[
            "-xJf",
            &tarball_str,
            "-C",
            &repo_str,
            "--strip-components=1",
        ],
        repo_dir,
        &[],
    )
    .await?;
    Ok(())
}

/// Extract the top-level `vendor/` directory from `tarball` into `repo_dir`.
///
/// When `replace` is `true` the existing `vendor/` directory is removed first
/// (clean replace per the backporting runbook § 3.3.3); otherwise the archive
/// is extracted over the existing tree (merge).
pub async fn overlay_vendor_dir(tarball: &Path, repo_dir: &Path, replace: bool) -> Result<()> {
    let vendor_dir = repo_dir.join("vendor");
    if replace && vendor_dir.is_dir() {
        std::fs::remove_dir_all(&vendor_dir).map_err(ThermiteError::Io)?;
    }

    let tarball_str = tarball.to_string_lossy().to_string();
    let repo_str = repo_dir.to_string_lossy().to_string();
    run_command(
        "tar",
        &["-xJf", &tarball_str, "-C", &repo_str],
        repo_dir,
        &[],
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "thermite-overlay-{label}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn create_tar_xz(src_dir: &Path, entry: &str, dest: &Path) {
        let status = Command::new("tar")
            .args(["-cJf"])
            .arg(dest)
            .arg("-C")
            .arg(src_dir)
            .arg(entry)
            .status()
            .expect("tar should be available");
        assert!(status.success(), "tar -cJf should succeed");
    }

    #[tokio::test]
    async fn overlay_orig_tarball_strips_top_level_dir() {
        let tmp = temp_dir("orig");
        let src_dir = tmp.join("src");
        let top = src_dir.join("rustc-1.85.0-src");
        fs::create_dir_all(top.join("src/compiler")).unwrap();
        fs::write(top.join("Cargo.toml"), "[package]").unwrap();
        fs::write(top.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(top.join("src/compiler/lib.rs"), "pub struct X;").unwrap();

        let tarball = tmp.join("rustc-1.85_1.85.0+dfsg.orig.tar.xz");
        create_tar_xz(&src_dir, "rustc-1.85.0-src", &tarball);

        let repo = tmp.join("rustc");
        fs::create_dir_all(&repo).unwrap();

        overlay_orig_tarball(&tarball, &repo)
            .await
            .expect("orig overlay should succeed");

        assert!(repo.join("Cargo.toml").exists(), "files land in repo root");
        assert!(repo.join("src/main.rs").exists());
        assert!(
            repo.join("src/compiler/lib.rs").exists(),
            "nested files are extracted"
        );
        assert!(
            !repo.join("rustc-1.85.0-src").exists(),
            "top-level dir must be stripped"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn overlay_vendor_dir_merges_without_replace() {
        let tmp = temp_dir("vendor-merge");
        let src_dir = tmp.join("src");
        fs::create_dir_all(src_dir.join("vendor")).unwrap();
        fs::write(src_dir.join("vendor").join("fresh.txt"), "fresh").unwrap();

        let tarball = tmp.join("rustc-1.85_1.85.0+dfsg.orig-vendor.tar.xz");
        create_tar_xz(&src_dir, "vendor", &tarball);

        let repo = tmp.join("rustc");
        fs::create_dir_all(repo.join("vendor")).unwrap();
        fs::write(repo.join("vendor").join("stale.txt"), "stale").unwrap();

        overlay_vendor_dir(&tarball, &repo, false)
            .await
            .expect("vendor merge overlay should succeed");

        assert!(
            repo.join("vendor/stale.txt").exists(),
            "merge must keep pre-existing files"
        );
        assert!(repo.join("vendor/fresh.txt").exists());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn overlay_vendor_dir_replaces_with_replace() {
        let tmp = temp_dir("vendor-replace");
        let src_dir = tmp.join("src");
        fs::create_dir_all(src_dir.join("vendor")).unwrap();
        fs::write(src_dir.join("vendor").join("fresh.txt"), "fresh").unwrap();

        let tarball = tmp.join("rustc-1.85_1.85.0+dfsg.orig-vendor.tar.xz");
        create_tar_xz(&src_dir, "vendor", &tarball);

        let repo = tmp.join("rustc");
        fs::create_dir_all(repo.join("vendor")).unwrap();
        fs::write(repo.join("vendor").join("stale.txt"), "stale").unwrap();

        overlay_vendor_dir(&tarball, &repo, true)
            .await
            .expect("vendor replace overlay should succeed");

        assert!(
            !repo.join("vendor/stale.txt").exists(),
            "replace must remove pre-existing files"
        );
        assert!(repo.join("vendor/fresh.txt").exists());

        let _ = fs::remove_dir_all(&tmp);
    }
}
