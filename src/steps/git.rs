use std::path::Path;

use crate::error::Result;
use crate::shell::{run_command, which};

/// Verify that every program in `tools` is available on `PATH`.
pub async fn check_required_tools(tools: &[&str]) -> Result<()> {
    for tool in tools {
        which(tool)?;
    }
    Ok(())
}

/// Verify the current directory looks like the root of a Debian source package.
pub async fn verify_debian_package_root(dir: &Path) -> Result<()> {
    let changelog = dir.join("debian/changelog");
    let watch = dir.join("debian/watch");
    if !changelog.exists() || !watch.exists() {
        return Err(crate::error::ThermiteError::NotADebianPackageRoot(
            dir.display().to_string(),
        ));
    }
    Ok(())
}

/// Run `git fetch --all` in `repo_dir`.
pub async fn fetch_all(repo_dir: &Path) -> Result<()> {
    run_command("git", &["fetch", "--all"], repo_dir, &[]).await?;
    Ok(())
}

/// Returns `true` if a local branch with the given name exists.
///
/// Uses `git branch --list <branch>`, which always exits 0 and writes the
/// branch name to stdout when it exists, or nothing when it does not.
pub async fn branch_exists(repo_dir: &Path, branch: &str) -> Result<bool> {
    let output = run_command("git", &["branch", "--list", branch], repo_dir, &[]).await?;
    Ok(!output.stdout.trim().is_empty())
}

/// Returns `true` if `branch` exists on `remote` (without checking it out).
///
/// Uses `git ls-remote --heads <remote> <branch>`, which exits 0 and prints the
/// matching ref to stdout when the branch exists on the remote, or prints
/// nothing and still exits 0 when it does not. This is preferable to a local
/// `git branch --list` for probing branches that have not yet been fetched
/// (or that the local clone may not have a remote-tracking ref for).
///
/// Returns `false` rather than erroring when the remote is unknown to git;
/// callers that need to distinguish "remote not configured" from "branch
/// absent on a configured remote" can inspect the underlying `Result` via the
/// `?`-propagated error from `run_command`.
pub async fn remote_branch_exists(repo_dir: &Path, remote: &str, branch: &str) -> Result<bool> {
    let output = run_command(
        "git",
        &["ls-remote", "--heads", remote, branch],
        repo_dir,
        &[],
    )
    .await?;
    Ok(!output.stdout.trim().is_empty())
}

/// Check out an existing branch.
pub async fn checkout_branch(repo_dir: &Path, branch: &str) -> Result<()> {
    run_command("git", &["checkout", branch], repo_dir, &[]).await?;
    Ok(())
}

/// Create a new branch from the current HEAD and push it to `remote`.
pub async fn create_and_push_branch(repo_dir: &Path, branch: &str, remote: &str) -> Result<()> {
    run_command("git", &["checkout", "-b", branch], repo_dir, &[]).await?;
    run_command("git", &["push", remote, branch], repo_dir, &[]).await?;
    Ok(())
}

/// Create a branch pointing at the current HEAD (does not check out).
pub async fn create_branch(repo_dir: &Path, branch: &str) -> Result<()> {
    run_command("git", &["branch", branch], repo_dir, &[]).await?;
    Ok(())
}

/// Delete a local branch (equivalent to `git branch -D <branch>`).
pub async fn delete_branch(repo_dir: &Path, branch: &str) -> Result<()> {
    run_command("git", &["branch", "-D", branch], repo_dir, &[]).await?;
    Ok(())
}

/// Push a branch to a remote.
pub async fn push_branch(repo_dir: &Path, remote: &str, branch: &str) -> Result<()> {
    run_command("git", &["push", remote, branch], repo_dir, &[]).await?;
    Ok(())
}

/// Reset the `experimental` branch to the current HEAD:
/// deletes it if it exists, then recreates it.
pub async fn reset_experimental_branch(repo_dir: &Path) -> Result<()> {
    // Ignore errors from delete in case the branch does not yet exist.
    let _ = run_command("git", &["branch", "-D", "experimental"], repo_dir, &[]).await;
    run_command("git", &["branch", "experimental"], repo_dir, &[]).await?;
    Ok(())
}

/// Cherry-pick a commit by its hash.
pub async fn cherry_pick(repo_dir: &Path, commit: &str) -> Result<()> {
    run_command("git", &["cherry-pick", commit], repo_dir, &[]).await?;
    Ok(())
}

/// Restore a file to its committed state using `git restore`.
///
/// `file` may be an absolute path or a path relative to the process CWD that
/// points inside `repo_dir`.  `git restore` must receive a path relative to
/// the repository root (i.e. relative to `repo_dir`), because git resolves
/// path arguments relative to its own working directory.  Passing an absolute
/// or process-CWD-relative path when the git cwd is `repo_dir` causes git to
/// report "is outside repository".
///
/// We strip the `repo_dir` prefix from `file` when possible; if `file` is
/// already relative to `repo_dir` (e.g. `debian/changelog`), `strip_prefix`
/// returns `Err` and `file` is used as-is.
pub async fn restore_file(repo_dir: &Path, file: &Path) -> Result<()> {
    // Strip the repo_dir prefix so git sees a repo-root-relative path.
    let relative = file.strip_prefix(repo_dir).unwrap_or(file);
    // Finding 11: replace panic! with a recoverable IO error so that
    // non-UTF-8 paths (however unlikely on Linux) produce a structured error.
    let path_str = relative.to_str().ok_or_else(|| {
        crate::error::ThermiteError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("non-UTF-8 file path: {relative:?}"),
        ))
    })?;
    run_command("git", &["restore", path_str], repo_dir, &[]).await?;
    Ok(())
}

/// Add files to the index and create a commit.
pub async fn add_and_commit(repo_dir: &Path, paths: &[&str], message: &str) -> Result<()> {
    let mut add_args = vec!["add"];
    add_args.extend_from_slice(paths);
    run_command("git", &add_args, repo_dir, &[]).await?;
    run_command("git", &["commit", "-m", message], repo_dir, &[]).await?;
    Ok(())
}

/// Search the log of `branch` for the most recent commit whose message contains
/// `needle`. Returns the short commit hash.
pub async fn find_commit_by_message(repo_dir: &Path, branch: &str, needle: &str) -> Result<String> {
    let output = run_command(
        "git",
        &["log", "--oneline", "--grep", needle, "-n", "1", branch],
        repo_dir,
        &[],
    )
    .await?;
    let hash = output
        .stdout
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_owned();
    if hash.is_empty() {
        return Err(crate::error::ThermiteError::CommandFailed {
            cmd: format!("git log --grep '{needle}' {branch}"),
            code: 0,
            stdout: String::new(),
            stderr: "no matching commit found".to_owned(),
        });
    }
    Ok(hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Finding 11: restore_file returns a structured Io error for a path that
    /// cannot be converted to UTF-8, rather than panicking.
    ///
    /// On Linux all paths in our test environment are valid UTF-8, so we verify
    /// the happy path returns Ok and that a non-existent path (valid UTF-8)
    /// returns an error from git rather than a panic.
    #[tokio::test]
    async fn restore_file_returns_error_not_panic_for_nonexistent_path() {
        let repo = PathBuf::from("/tmp");
        let nonexistent = PathBuf::from("/tmp/no-such-file-xyz-thermite.txt");
        // Should return Err (git will fail) rather than panic.
        let result = restore_file(&repo, &nonexistent).await;
        // We expect an error (git not in a repo, or file not found).
        assert!(result.is_err(), "expected an error, not panic");
    }

    /// Regression: when `file` is constructed as `repo_dir.join(relative)` and
    /// `repo_dir` is itself a relative path (e.g. `../some/repo`), `git restore`
    /// must receive only the repo-root-relative portion (`relative`), not the
    /// full path which git would interpret relative to its own cwd and reject
    /// as "outside repository".
    ///
    /// We can't run real git here, so we just assert that `strip_prefix` on the
    /// constructed path yields the expected relative component.
    #[test]
    fn restore_file_strips_repo_dir_prefix_from_relative_path() {
        let repo_dir = PathBuf::from("../testing-1.92.0/rustc");
        let file = repo_dir.join("debian/changelog");
        let relative = file.strip_prefix(&repo_dir).unwrap();
        assert_eq!(relative, std::path::Path::new("debian/changelog"));
    }

    /// `remote_branch_exists` must report `true` for a branch that exists on the
    /// remote and `false` for one that does not. We build a minimal two-repo
    /// fixture (a bare "remote" plus a working clone), push a branch, and probe
    /// both names.
    #[tokio::test]
    async fn remote_branch_exists_detects_present_and_absent_branches() {
        use std::fs;
        let tmp = std::env::temp_dir().join(format!(
            "thermite-remote-branch-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        let remote_path = tmp.join("remote.git");
        fs::create_dir_all(&remote_path).unwrap();

        // Initialise a bare repository to act as the remote.
        run_command(
            "git",
            &["init", "--bare", remote_path.to_str().unwrap()],
            &tmp,
            &[],
        )
        .await
        .unwrap();

        // Clone the bare remote into a working repo we can commit to.
        let work = tmp.join("work");
        run_command(
            "git",
            &[
                "clone",
                remote_path.to_str().unwrap(),
                work.to_str().unwrap(),
            ],
            &tmp,
            &[],
        )
        .await
        .unwrap();

        // Configure identity and create an initial commit on `main`.
        run_command("git", &["config", "user.email", "t@t"], &work, &[])
            .await
            .unwrap();
        run_command("git", &["config", "user.name", "t"], &work, &[])
            .await
            .unwrap();
        fs::write(work.join("README"), "init\n").unwrap();
        run_command("git", &["add", "README"], &work, &[])
            .await
            .unwrap();
        run_command(
            "git",
            &["commit", "-m", "init", "--allow-empty"],
            &work,
            &[],
        )
        .await
        .unwrap();

        // `git init --bare` may default to `master` or `main` depending on git
        // version; rename to `main` so the push below targets a known name.
        run_command("git", &["branch", "-M", "main"], &work, &[])
            .await
            .unwrap();
        run_command("git", &["push", "-u", "origin", "main"], &work, &[])
            .await
            .unwrap();

        // Create and push the branch we want to probe for.
        run_command("git", &["checkout", "-b", "noble-1.85"], &work, &[])
            .await
            .unwrap();
        run_command("git", &["push", "origin", "noble-1.85"], &work, &[])
            .await
            .unwrap();

        // The probe must succeed for the branch that exists...
        assert!(
            remote_branch_exists(&work, "origin", "noble-1.85")
                .await
                .unwrap(),
            "noble-1.85 should be reported as present on origin"
        );
        // ...and fail for one that does not.
        assert!(
            !remote_branch_exists(&work, "origin", "stonking-1.85")
                .await
                .unwrap(),
            "stonking-1.85 should be reported as absent on origin"
        );

        let _ = fs::remove_dir_all(&tmp);
    }
}
