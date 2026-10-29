use thiserror::Error;

/// The unified error type for thermite operations.
#[derive(Debug, Error)]
pub enum ThermiteError {
    #[error("invalid Rust version string '{0}': expected X.Y.Z format")]
    InvalidRustVersion(String),

    #[error("unknown Ubuntu release '{0}'")]
    UnknownRelease(String),

    /// A child process exited with a non-zero status.
    ///
    /// Both `stdout` and `stderr` are captured so that callers can
    /// parse tool-specific diagnostics regardless of which stream the
    /// tool chose to write to.
    #[error("command '{cmd}' failed with exit code {code}:\n{stderr}")]
    CommandFailed {
        cmd: String,
        code: i32,
        /// Captured standard output of the failed command.
        stdout: String,
        stderr: String,
    },

    #[error("command '{0}' was not found on PATH")]
    CommandNotFound(String),

    #[error("command '{0}' output could not be interpreted as a string")]
    CommandOutputParseError(String),

    #[error("patch refresh required manual intervention: {0}")]
    PatchRefreshRequired(String),

    #[error("debian package root not found at '{0}': missing debian/changelog or debian/watch")]
    NotADebianPackageRoot(String),

    /// A `debian/changelog` file was expected at a path but does not exist.
    ///
    /// Raised when the target directory does not look like a Debian source
    /// package (e.g. `thermite version` / `thermite tui` run outside a
    /// packaging repo), so the user gets a hint instead of a bare
    /// "No such file or directory".
    #[error("not a Debian source package: missing debian/changelog at '{0}'")]
    MissingChangelog(String),

    /// Launchpad username was empty or otherwise invalid.
    #[error("invalid Launchpad username: {0}")]
    InvalidLpUser(String),

    /// Launchpad bug number was not a non-empty digit string.
    #[error("invalid Launchpad bug number '{0}': must be a non-empty string of digits")]
    InvalidLpBugNumber(String),

    /// Source and target releases for a backport were the same.
    #[error("invalid backport releases: {0}")]
    InvalidBackportReleases(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, ThermiteError>;

#[cfg(test)]
mod tests {
    use super::*;

    /// Finding 1: ThermiteError::CommandFailed must carry both stdout and stderr.
    #[test]
    fn command_failed_carries_stdout_and_stderr() {
        let err = ThermiteError::CommandFailed {
            cmd: "test-cmd".to_owned(),
            code: 1,
            stdout: "stdout content".to_owned(),
            stderr: "stderr content".to_owned(),
        };
        // Display should reference the command.
        assert!(err.to_string().contains("test-cmd"));
        // stdout is accessible via pattern matching.
        if let ThermiteError::CommandFailed { stdout, stderr, .. } = &err {
            assert_eq!(stdout, "stdout content");
            assert_eq!(stderr, "stderr content");
        } else {
            panic!("expected CommandFailed variant");
        }
    }

    /// Finding 10: Dedicated error variant for invalid Launchpad username.
    #[test]
    fn invalid_lp_user_variant_exists() {
        let err = ThermiteError::InvalidLpUser("empty-user".to_owned());
        assert!(err.to_string().contains("empty-user"));
        assert!(matches!(err, ThermiteError::InvalidLpUser(_)));
    }

    /// Finding 10: Dedicated error variant for invalid Launchpad bug number.
    #[test]
    fn invalid_lp_bug_number_variant_exists() {
        let err = ThermiteError::InvalidLpBugNumber("abc".to_owned());
        assert!(err.to_string().contains("abc"));
        assert!(matches!(err, ThermiteError::InvalidLpBugNumber(_)));
    }
}
