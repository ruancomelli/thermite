use crate::error::{Result, ThermiteError};
use crate::types::{ubuntu::UbuntuRelease, versions::RustVersion};

/// Parameters for the `thermite backport` command.
///
/// All values are validated on construction.
#[derive(Debug, Clone)]
pub struct BackportParams {
    /// Full Rust version being backported, e.g. `"1.85.0"`.
    pub rust_version: RustVersion,
    /// Ubuntu release being backported **from**, e.g. `"noble"`.
    pub source_release: UbuntuRelease,
    /// `true` when the user passed `--source-release devel` and the value was
    /// resolved to the current devel release
    /// ([`UbuntuRelease::devel_release`]). `false` for a concrete adjective.
    pub source_release_is_devel_alias: bool,
    /// Ubuntu release being backported **to**, e.g. `"jammy"`.
    pub release: UbuntuRelease,
    /// Launchpad username; also used as the personal Git remote name.
    pub lpuser: String,
    /// Local Git remote name for the Foundations rustc repository.
    /// Defaults to `"foundations"`.
    pub git_remote: String,
    /// Launchpad bug ID number for this work (digits only).
    /// `None` for proactive backports that do not have an associated bug.
    pub lp_bug_number: Option<String>,
    /// When `true`, skip all hard-to-revert operations (remote git push, PPA
    /// creation, dput uploads) and print what each would have done instead.
    pub dry_run: bool,
}

impl BackportParams {
    /// Construct and validate a new [`BackportParams`].
    ///
    /// `lp_bug_number` is `None` for proactive backports.  When `Some`, it
    /// must be a non-empty string of ASCII digits.
    ///
    /// `dry_run` — when `true`, all hard-to-revert operations (remote git
    /// push, PPA creation, dput uploads) are printed but not executed.
    pub fn new(
        rust_version: &str,
        source_release: &str,
        release: &str,
        lpuser: &str,
        git_remote: &str,
        lp_bug_number: Option<&str>,
        dry_run: bool,
    ) -> Result<Self> {
        if lpuser.is_empty() {
            return Err(ThermiteError::InvalidLpUser(
                "lpuser must not be empty".to_owned(),
            ));
        }
        if let Some(bug) = lp_bug_number
            && (bug.is_empty() || !bug.chars().all(|c| c.is_ascii_digit()))
        {
            return Err(ThermiteError::InvalidLpBugNumber(format!(
                "'{bug}' must be a non-empty string of digits"
            )));
        }
        let source = if source_release.eq_ignore_ascii_case("devel") {
            UbuntuRelease::devel_release()
        } else {
            UbuntuRelease::parse(source_release)?
        };
        let source_release_is_devel_alias = source_release.eq_ignore_ascii_case("devel");
        // The target release must be a concrete adjective; "devel" is not a
        // valid backport target.
        if release.eq_ignore_ascii_case("devel") {
            return Err(ThermiteError::InvalidBackportReleases(
                "cannot backport to 'devel'; specify a concrete target release \
                 (e.g. 'resolute', 'noble')"
                    .to_owned(),
            ));
        }
        let target = UbuntuRelease::parse(release)?;
        if source == target {
            return Err(ThermiteError::InvalidBackportReleases(format!(
                "source release and target release must differ (both are '{release}')"
            )));
        }
        Ok(Self {
            rust_version: RustVersion::parse(rust_version)?,
            source_release: source,
            source_release_is_devel_alias,
            release: target,
            lpuser: lpuser.to_owned(),
            git_remote: git_remote.to_owned(),
            lp_bug_number: lp_bug_number.map(|s| s.to_owned()),
            dry_run,
        })
    }
}

#[cfg(test)]
mod backport_params_tests {
    use super::*;
    use crate::error::ThermiteError;

    fn valid_backport() -> BackportParams {
        BackportParams::new(
            "1.85.0",
            "noble",
            "jammy",
            "jdoe",
            "foundations",
            None,
            false,
        )
        .unwrap()
    }

    #[test]
    fn valid_backport_params_constructs_successfully() {
        let p = valid_backport();
        assert_eq!(p.lpuser, "jdoe");
        assert_eq!(p.lp_bug_number, None);
    }

    #[test]
    fn backport_with_bug_number_accepted() {
        let p = BackportParams::new(
            "1.85.0",
            "noble",
            "jammy",
            "jdoe",
            "foundations",
            Some("99999"),
            false,
        )
        .unwrap();
        assert_eq!(p.lp_bug_number, Some("99999".to_owned()));
    }

    #[test]
    fn non_digit_bug_number_returns_error() {
        let result = BackportParams::new(
            "1.85.0",
            "noble",
            "jammy",
            "jdoe",
            "foundations",
            Some("abc"),
            false,
        );
        assert!(
            matches!(result, Err(ThermiteError::InvalidLpBugNumber(_))),
            "expected InvalidLpBugNumber, got: {result:?}"
        );
    }

    #[test]
    fn empty_bug_number_returns_error() {
        let result = BackportParams::new(
            "1.85.0",
            "noble",
            "jammy",
            "jdoe",
            "foundations",
            Some(""),
            false,
        );
        assert!(
            matches!(result, Err(ThermiteError::InvalidLpBugNumber(_))),
            "expected InvalidLpBugNumber, got: {result:?}"
        );
    }

    #[test]
    fn same_source_and_target_release_returns_error() {
        let result = BackportParams::new(
            "1.85.0",
            "noble",
            "noble",
            "jdoe",
            "foundations",
            None,
            false,
        );
        assert!(
            matches!(result, Err(ThermiteError::InvalidBackportReleases(_))),
            "expected InvalidBackportReleases, got: {result:?}"
        );
    }

    #[test]
    fn empty_lpuser_returns_invalid_lp_user_error() {
        let result =
            BackportParams::new("1.85.0", "noble", "jammy", "", "foundations", None, false);
        assert!(
            matches!(result, Err(ThermiteError::InvalidLpUser(_))),
            "expected InvalidLpUser, got: {result:?}"
        );
    }

    #[test]
    fn dry_run_flag_is_stored() {
        let p = BackportParams::new(
            "1.85.0",
            "noble",
            "jammy",
            "jdoe",
            "foundations",
            None,
            true,
        )
        .unwrap();
        assert!(p.dry_run, "dry_run should be stored as true");

        let p2 = BackportParams::new(
            "1.85.0",
            "noble",
            "jammy",
            "jdoe",
            "foundations",
            None,
            false,
        )
        .unwrap();
        assert!(!p2.dry_run, "dry_run should be stored as false");
    }

    #[test]
    fn devel_as_source_resolves_to_current_devel_release() {
        let p = BackportParams::new(
            "1.85.0",
            "devel",
            "resolute",
            "jdoe",
            "foundations",
            None,
            false,
        )
        .unwrap();
        assert_eq!(p.source_release.as_str(), UbuntuRelease::devel());
        assert!(
            p.source_release_is_devel_alias,
            "source_release_is_devel_alias should be true for 'devel'"
        );
    }

    #[test]
    fn concrete_source_release_does_not_set_devel_alias_flag() {
        let p = BackportParams::new(
            "1.85.0",
            "resolute",
            "noble",
            "jdoe",
            "foundations",
            None,
            false,
        )
        .unwrap();
        assert!(!p.source_release_is_devel_alias);
    }

    #[test]
    fn devel_as_target_is_rejected() {
        let result = BackportParams::new(
            "1.85.0",
            "resolute",
            "devel",
            "jdoe",
            "foundations",
            None,
            false,
        );
        assert!(
            matches!(result, Err(ThermiteError::InvalidBackportReleases(_))),
            "expected InvalidBackportReleases for 'devel' target, got: {result:?}"
        );
    }

    #[test]
    fn devel_source_and_devel_target_is_rejected() {
        let result = BackportParams::new(
            "1.85.0",
            "devel",
            "devel",
            "jdoe",
            "foundations",
            None,
            false,
        );
        assert!(
            matches!(result, Err(ThermiteError::InvalidBackportReleases(_))),
            "expected InvalidBackportReleases for 'devel' target, got: {result:?}"
        );
    }
}

/// The action requested from the `thermite tarball` command suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TarballAction {
    /// Fetch the tarball from the staging PPA / Ubuntu archive when missing.
    Download,
    /// Produce the tarball locally (uscan / `debian/rules vendor-tarball`).
    Generate,
    /// Extract an existing tarball's contents into the repo directory.
    Overlay,
}

/// Parameters for the `thermite tarball` command suite.
///
/// All values are validated on construction.
#[derive(Debug, Clone)]
pub struct TarballParams {
    /// Action requested for the targeted tarballs.
    pub action: TarballAction,
    /// Full Rust version the tarballs are named after, e.g. `"1.85.0"`.
    pub rust_version: RustVersion,
    /// Target Ubuntu release adjective used for backport naming
    /// (e.g. `noble` → `+dfsg~26.04` tarball names). `None` selects the
    /// plain update naming (`+dfsg`, no series suffix).
    pub series: Option<UbuntuRelease>,
    /// When `true`, `generate` may overwrite tarballs that already exist in
    /// the parent directory. Without it, `generate` refuses and points at the
    /// existing file.
    pub force: bool,
    /// Only meaningful for the `Overlay` action: when `true`, overlaying the
    /// vendor tarball first removes the existing `vendor/` directory (clean
    /// replace per the runbook) instead of merging over it.
    pub overlay_replace: bool,
}

impl TarballParams {
    /// Construct and validate a new [`TarballParams`].
    ///
    /// `series` is an Ubuntu release adjective (e.g. `"noble"`) or `None` for
    /// plain update naming.
    pub fn new(
        action: TarballAction,
        rust_version: &str,
        series: Option<&str>,
        force: bool,
        overlay_replace: bool,
    ) -> Result<Self> {
        let series = match series {
            None => None,
            Some(s) if s.eq_ignore_ascii_case("devel") => {
                return Err(ThermiteError::InvalidBackportReleases(
                    "cannot use 'devel' as --series; specify a concrete release \
                     (e.g. 'resolute', 'noble')"
                        .to_owned(),
                ));
            }
            Some(s) => Some(UbuntuRelease::parse(s)?),
        };
        Ok(Self {
            action,
            rust_version: RustVersion::parse(rust_version)?,
            series,
            force,
            overlay_replace,
        })
    }

    /// The `+dfsg` suffix component used in tarball filenames: `""` for plain
    /// update naming or `"~<series>"` (e.g. `"~26.04"`) for backport naming.
    pub fn dfsg_suffix(&self) -> String {
        match &self.series {
            None => String::new(),
            Some(r) => format!("~{}", r.series_number()),
        }
    }
}

#[cfg(test)]
mod tarball_params_tests {
    use super::*;

    fn valid_params() -> TarballParams {
        TarballParams::new(TarballAction::Download, "1.85.0", None, false, false).unwrap()
    }

    #[test]
    fn valid_tarball_params_constructs_successfully() {
        let p = valid_params();
        assert_eq!(p.rust_version, RustVersion::parse("1.85.0").unwrap());
        assert_eq!(p.series, None);
        assert_eq!(p.action, TarballAction::Download);
        assert!(!p.force);
        assert!(!p.overlay_replace);
    }

    #[test]
    fn dfsg_suffix_empty_without_series() {
        assert_eq!(valid_params().dfsg_suffix(), "");
    }

    #[test]
    fn dfsg_suffix_contains_series_number() {
        let p = TarballParams::new(
            TarballAction::Generate,
            "1.85.0",
            Some("noble"),
            false,
            false,
        )
        .unwrap();
        assert_eq!(p.dfsg_suffix(), "~24.04");
    }

    #[test]
    fn invalid_series_release_rejected() {
        let result = TarballParams::new(
            TarballAction::Generate,
            "1.85.0",
            Some("bogus"),
            false,
            false,
        );
        assert!(
            matches!(result, Err(ThermiteError::UnknownRelease(_))),
            "expected UnknownRelease, got: {result:?}"
        );
    }

    #[test]
    fn devel_series_rejected() {
        let result = TarballParams::new(
            TarballAction::Generate,
            "1.85.0",
            Some("devel"),
            false,
            false,
        );
        assert!(
            matches!(result, Err(ThermiteError::InvalidBackportReleases(_))),
            "expected InvalidBackportReleases, got: {result:?}"
        );
    }

    #[test]
    fn invalid_rust_version_rejected() {
        let result = TarballParams::new(TarballAction::Generate, "1.85", None, false, false);
        assert!(
            matches!(result, Err(ThermiteError::InvalidRustVersion(_))),
            "expected InvalidRustVersion, got: {result:?}"
        );
    }

    #[test]
    fn flags_are_stored() {
        let p = TarballParams::new(TarballAction::Generate, "1.85.0", Some("noble"), true, true)
            .unwrap();
        assert_eq!(p.action, TarballAction::Generate);
        assert!(p.force);
        assert!(p.overlay_replace);
    }

    #[test]
    fn overlay_action_is_stored() {
        let p = TarballParams::new(TarballAction::Overlay, "1.85.0", None, false, true).unwrap();
        assert_eq!(p.action, TarballAction::Overlay);
        assert!(p.overlay_replace);
    }
}

/// Parameters for the `thermite update` command.
///
/// All values are validated on construction. The short version fields are
/// derived automatically from their full-version counterparts.
#[derive(Debug, Clone)]
pub struct UpdateParams {
    /// Full Rust version being packaged, e.g. `"1.85.1"`.
    pub rust_update_version: RustVersion,
    /// Full Rust version being replaced, e.g. `"1.84.0"`.
    pub rust_old_version: RustVersion,
    /// Target Ubuntu release adjective, e.g. `"noble"`.
    pub release: UbuntuRelease,
    /// Launchpad username; also used as the personal Git remote name.
    pub lpuser: String,
    /// Local Git remote name for the Foundations rustc repository.
    /// Defaults to `"foundations"`.
    pub git_remote: String,
    /// Launchpad bug ID number for this work (digits only).
    pub lp_bug_number: String,
}

impl UpdateParams {
    /// Construct and validate a new [`UpdateParams`].
    pub fn new(
        rust_update_version: &str,
        rust_old_version: &str,
        release: &str,
        lpuser: &str,
        git_remote: &str,
        lp_bug_number: &str,
    ) -> Result<Self> {
        if lpuser.is_empty() {
            // Finding 10: use the dedicated variant instead of InvalidRustVersion.
            return Err(ThermiteError::InvalidLpUser(
                "lpuser must not be empty".to_owned(),
            ));
        }
        if lp_bug_number.is_empty() || !lp_bug_number.chars().all(|c| c.is_ascii_digit()) {
            // Finding 10: dedicated variant for bug number validation failure.
            return Err(ThermiteError::InvalidLpBugNumber(format!(
                "'{lp_bug_number}' must be a non-empty string of digits"
            )));
        }
        Ok(Self {
            rust_update_version: RustVersion::parse(rust_update_version)?,
            rust_old_version: RustVersion::parse(rust_old_version)?,
            release: UbuntuRelease::parse(release)?,
            lpuser: lpuser.to_owned(),
            git_remote: git_remote.to_owned(),
            lp_bug_number: lp_bug_number.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ThermiteError;

    fn valid_params() -> UpdateParams {
        UpdateParams::new("1.85.1", "1.84.0", "noble", "jdoe", "foundations", "12345").unwrap()
    }

    #[test]
    fn valid_params_constructs_successfully() {
        let p = valid_params();
        assert_eq!(p.lpuser, "jdoe");
        assert_eq!(p.lp_bug_number, "12345");
    }

    /// Finding 10: empty lpuser must return InvalidLpUser, not InvalidRustVersion.
    #[test]
    fn empty_lpuser_returns_invalid_lp_user_error() {
        let result = UpdateParams::new("1.85.1", "1.84.0", "noble", "", "foundations", "12345");
        assert!(
            matches!(result, Err(ThermiteError::InvalidLpUser(_))),
            "expected InvalidLpUser, got: {:?}",
            result
        );
    }

    /// Finding 10: non-digit lp_bug_number must return InvalidLpBugNumber.
    #[test]
    fn non_digit_lp_bug_number_returns_invalid_lp_bug_number_error() {
        let result = UpdateParams::new("1.85.1", "1.84.0", "noble", "jdoe", "foundations", "abc");
        assert!(
            matches!(result, Err(ThermiteError::InvalidLpBugNumber(_))),
            "expected InvalidLpBugNumber, got: {:?}",
            result
        );
    }

    /// Finding 10: empty lp_bug_number must also return InvalidLpBugNumber.
    #[test]
    fn empty_lp_bug_number_returns_invalid_lp_bug_number_error() {
        let result = UpdateParams::new("1.85.1", "1.84.0", "noble", "jdoe", "foundations", "");
        assert!(
            matches!(result, Err(ThermiteError::InvalidLpBugNumber(_))),
            "expected InvalidLpBugNumber, got: {:?}",
            result
        );
    }
}
