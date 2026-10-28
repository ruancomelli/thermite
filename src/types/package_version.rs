//! Structured representation of an Ubuntu `rustc-X.Y` Debian package version string.
//!
//! The NEW format (as documented in the Ubuntu project docs) is:
//!
//! **Non-backport:**
//! ```text
//! <upstream_version>+dfsg[<repack_number>]-0ubuntu<ubuntu_revision>
//! ```
//!
//! **Backport:**
//! ```text
//! <upstream_version>+dfsg[<repack_number>]~<series>[.<backport_repack>]-0ubuntu<ubuntu_revision>~<series>.<backport_revision>
//! ```
//!
//! **Stage0 bootstrap:**
//! ```text
//! <upstream_version>+dfsg[<repack_number>]~<series>~stage0-0ubuntu<ubuntu_revision>~<series>.<backport_revision>
//! ```

use std::fmt;

use serde::Serialize;

use crate::error::{Result, ThermiteError};
use crate::types::ubuntu::UbuntuRelease;
use crate::types::versions::RustVersion;

/// A fully parsed Ubuntu `rustc` package version string.
///
/// This type covers the NEW versioning schema used by the Foundations team.
/// It can represent non-backport versions, backport versions, and stage0
/// bootstrap versions. It also handles the optional `~ppaN` development suffix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustcPackageVersion {
    /// The upstream Rust toolchain version (e.g. `1.85.0`).
    pub upstream: RustVersion,

    /// The dfsg repack number. `None` means bare `+dfsg` (initial upload);
    /// `Some(1)` means `+dfsg1` (repacked once after initial upload), etc.
    pub repack_number: Option<u32>,

    /// The target Ubuntu series number for backports (e.g. `"24.04"`).
    /// `None` for non-backport versions.
    pub series: Option<String>,

    /// Additional repack counter for the backport's orig tarball.
    /// Corresponds to the `.<backport_repack>` after `~<series>` in the
    /// upstream part. `None` means no additional repack.
    pub backport_repack: Option<u32>,

    /// Whether this is a stage0 bootstrap package (`~stage0` marker).
    pub stage0: bool,

    /// The Ubuntu revision number (the `N` in `-0ubuntuN`).
    pub ubuntu_revision: u32,

    /// The backport revision number (the `N` in `~<series>.N`).
    /// Only present for backport versions.
    pub backport_revision: Option<u32>,

    /// The PPA upload number (the `N` in `~ppaN`).
    /// This is a development-time suffix used for iterating on PPA builds.
    /// `None` means no PPA suffix (release candidate or final upload).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ppa: Option<u32>,
}

impl RustcPackageVersion {
    /// Parse a full Debian version string into a structured representation.
    ///
    /// # Errors
    ///
    /// Returns `ThermiteError::InvalidRustVersion` if the string does not
    /// conform to the expected format.
    pub fn parse(s: &str) -> Result<Self> {
        // Split on the first `-` to separate upstream from debian revision.
        let (upstream_part, debian_rev) = s.split_once('-').ok_or_else(|| {
            ThermiteError::InvalidRustVersion(format!(
                "missing '-' separator in version string: {s}"
            ))
        })?;

        // --- Parse the upstream part ---
        // Format: <X.Y.Z>+dfsg[<repack_number>][~<series>[.<backport_repack>]][~stage0]
        let (before_dfsg, after_dfsg) = upstream_part.split_once("+dfsg").ok_or_else(|| {
            ThermiteError::InvalidRustVersion(format!(
                "missing '+dfsg' in upstream part: {upstream_part}"
            ))
        })?;

        // Parse the upstream version (X.Y.Z).
        let upstream = RustVersion::parse(before_dfsg)?;

        // Parse what comes after "+dfsg": [<repack_number>][~<series>[.<bp_repack>]][~stage0]
        let (repack_number, after_repack) = parse_repack_number(after_dfsg);

        // Check for ~stage0 and ~<series>[.<backport_repack>]
        let (series, backport_repack, stage0) = parse_upstream_suffixes(after_repack)?;

        // --- Parse the debian revision ---
        // Strip trailing ~ppaN suffix if present before parsing the rest.
        let (debian_rev_main, ppa) = strip_ppa_suffix(debian_rev);

        // Format: 0ubuntu<ubuntu_revision>[~<series>.<backport_revision>]
        let (ubuntu_revision, backport_revision) = parse_debian_revision(debian_rev_main, s)?;

        Ok(Self {
            upstream,
            repack_number,
            series,
            backport_repack,
            stage0,
            ubuntu_revision,
            backport_revision,
            ppa,
        })
    }

    /// Whether this version represents a backport.
    pub fn is_backport(&self) -> bool {
        self.series.is_some()
    }

    /// Whether this version represents a stage0 bootstrap package.
    pub fn is_stage0(&self) -> bool {
        self.stage0
    }

    /// Convert a non-backport version into a backport targeting `target_series`.
    ///
    /// - Sets `series` to the target series.
    /// - Resets `backport_repack` to `None`.
    /// - Resets `backport_revision` to `1`.
    /// - Clears `ppa` suffix.
    /// - Preserves `upstream`, `repack_number`, and `ubuntu_revision`.
    ///
    /// If the version is already a backport, this retargets it to the new
    /// series (equivalent to `retarget_series`).
    pub fn to_backport(&mut self, target_series: &str) {
        self.series = Some(target_series.to_owned());
        self.backport_repack = None;
        self.backport_revision = Some(1);
        self.stage0 = false;
        self.ppa = None;
    }

    /// Change the target series for an existing backport.
    ///
    /// Resets `backport_repack` and `backport_revision` to their initial state.
    pub fn retarget_series(&mut self, new_series: &str) {
        self.to_backport(new_series);
    }

    /// Bump the upstream version to a new patch release.
    ///
    /// Resets all other components to their initial state:
    /// - `repack_number` → `None` (bare `+dfsg`)
    /// - `series` → `None`
    /// - `backport_repack` → `None`
    /// - `stage0` → `false`
    /// - `ubuntu_revision` → `1`
    /// - `backport_revision` → `None`
    /// - `ppa` → `None`
    pub fn bump_patch_release(&mut self, new_upstream: RustVersion) {
        self.upstream = new_upstream;
        self.repack_number = None;
        self.series = None;
        self.backport_repack = None;
        self.stage0 = false;
        self.ubuntu_revision = 1;
        self.backport_revision = None;
        self.ppa = None;
    }

    /// Increment the Ubuntu revision number.
    pub fn bump_ubuntu_revision(&mut self) {
        self.ubuntu_revision += 1;
    }

    /// Increment the repack number and reset the Ubuntu revision to 1.
    ///
    /// If `repack_number` is `None` (bare `+dfsg`), it becomes `Some(1)`.
    /// Otherwise it is incremented.
    pub fn bump_repack(&mut self) {
        self.repack_number = Some(self.repack_number.map_or(1, |n| n + 1));
        self.ubuntu_revision = 1;
    }

    /// Increment the backport revision number.
    ///
    /// Only meaningful for backport versions. If `backport_revision` is `None`,
    /// it becomes `Some(2)` (assuming the initial upload was `1`).
    pub fn bump_backport_revision(&mut self) {
        self.backport_revision = Some(self.backport_revision.map_or(2, |n| n + 1));
    }

    /// Increment the backport repack number and reset the backport revision to 1.
    ///
    /// If `backport_repack` is `None`, it becomes `Some(1)`. Otherwise it is
    /// incremented.
    pub fn bump_backport_repack(&mut self) {
        self.backport_repack = Some(self.backport_repack.map_or(1, |n| n + 1));
        self.backport_revision = Some(1);
    }

    /// Mark this version as a stage0 bootstrap build.
    ///
    /// Only meaningful for backport versions (series must be set).
    pub fn set_stage0(&mut self, stage0: bool) {
        self.stage0 = stage0;
    }

    /// Construct a new `RustcPackageVersion` from its components (for `format`).
    pub fn new(
        upstream: RustVersion,
        repack_number: Option<u32>,
        series: Option<String>,
        backport_repack: Option<u32>,
        stage0: bool,
        ubuntu_revision: u32,
        backport_revision: Option<u32>,
    ) -> Self {
        Self {
            upstream,
            repack_number,
            series,
            backport_repack,
            stage0,
            ubuntu_revision,
            backport_revision,
            ppa: None,
        }
    }

    /// Set the PPA upload number.
    pub fn set_ppa(&mut self, ppa: Option<u32>) {
        self.ppa = ppa;
    }

    /// Increment the PPA number. If `None`, becomes `Some(1)`.
    pub fn bump_ppa(&mut self) {
        self.ppa = Some(self.ppa.map_or(1, |n| n + 1));
    }

    /// Remove the PPA suffix (for final archive upload).
    pub fn clear_ppa(&mut self) {
        self.ppa = None;
    }
}

impl fmt::Display for RustcPackageVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Upstream part: <X.Y.Z>+dfsg[<repack_number>]
        write!(f, "{}+dfsg", self.upstream)?;
        if let Some(n) = self.repack_number {
            write!(f, "{n}")?;
        }

        // Backport series suffix: ~<series>[.<backport_repack>]
        if let Some(series) = &self.series {
            write!(f, "~{series}")?;
            if let Some(bp_repack) = self.backport_repack {
                write!(f, ".{bp_repack}")?;
            }
        }

        // Stage0 marker
        if self.stage0 {
            write!(f, "~stage0")?;
        }

        // Debian revision: -0ubuntu<ubuntu_revision>
        write!(f, "-0ubuntu{}", self.ubuntu_revision)?;

        // Backport revision suffix: ~<series>.<backport_revision>
        if let Some(series) = &self.series {
            let bp_rev = self.backport_revision.unwrap_or(1);
            write!(f, "~{series}.{bp_rev}")?;
        }

        // PPA suffix: ~ppaN
        if let Some(ppa) = self.ppa {
            write!(f, "~ppa{ppa}")?;
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Bump operations
// ---------------------------------------------------------------------------

/// The bump operation to perform on a [`RustcPackageVersion`].
///
/// Shared by the `thermite version bump` CLI subcommand and the TUI
/// version-bump action. The operations that need a series accept either a
/// numeric series (`"24.04"`) or an Ubuntu release adjective (`"noble"`).
#[derive(Debug, Clone)]
pub enum BumpOperation {
    /// New upstream patch release — resets all fields.
    PatchRelease { upstream: String },
    /// Increment the Ubuntu revision.
    UbuntuRevision,
    /// Increment the repack number (resets Ubuntu revision).
    Repack,
    /// Generate a backport version (sets series, backport_revision=1).
    Backport {
        series: Option<String>,
        release: Option<String>,
    },
    /// Increment the backport revision.
    BackportRevision,
    /// Increment the backport repack (resets backport revision).
    BackportRepack,
    /// Retarget a backport to a different series.
    Retarget {
        series: Option<String>,
        release: Option<String>,
    },
    /// Increment the PPA number (for PPA upload iteration).
    Ppa,
    /// Remove the PPA suffix (for final archive upload).
    ClearPpa,
}

/// Resolve a series from either a direct series number (`"24.04"`) or an
/// Ubuntu release adjective (`"noble"`). The two inputs are mutually
/// exclusive.
pub fn resolve_series(series: Option<&str>, release: Option<&str>) -> Result<Option<String>> {
    match (series, release) {
        (Some(_), Some(_)) => Err(ThermiteError::InvalidRustVersion(
            "--series and --release are mutually exclusive".to_owned(),
        )),
        (Some(s), None) => Ok(Some(s.to_owned())),
        (None, Some(r)) => {
            let ubuntu_release = UbuntuRelease::parse(r)?;
            Ok(Some(ubuntu_release.series_number().to_owned()))
        }
        (None, None) => Ok(None),
    }
}

/// Apply `operation` to `v` in place.
///
/// # Errors
///
/// Returns `ThermiteError::InvalidRustVersion` when an operation requires a
/// series but neither `--series` nor `--release` was supplied, or when the
/// upstream version in a patch-release operation is not a valid
/// `X.Y.Z` version.
pub fn apply_bump_operation(v: &mut RustcPackageVersion, operation: &BumpOperation) -> Result<()> {
    match operation {
        BumpOperation::PatchRelease { upstream } => {
            let new_upstream = RustVersion::parse(upstream)?;
            v.bump_patch_release(new_upstream);
        }
        BumpOperation::UbuntuRevision => {
            v.bump_ubuntu_revision();
        }
        BumpOperation::Repack => {
            v.bump_repack();
        }
        BumpOperation::Backport { series, release } => {
            let resolved =
                resolve_series(series.as_deref(), release.as_deref())?.ok_or_else(|| {
                    ThermiteError::InvalidRustVersion(
                        "backport operation requires --series or --release".to_owned(),
                    )
                })?;
            v.to_backport(&resolved);
        }
        BumpOperation::BackportRevision => {
            v.bump_backport_revision();
        }
        BumpOperation::BackportRepack => {
            v.bump_backport_repack();
        }
        BumpOperation::Retarget { series, release } => {
            let resolved =
                resolve_series(series.as_deref(), release.as_deref())?.ok_or_else(|| {
                    ThermiteError::InvalidRustVersion(
                        "retarget operation requires --series or --release".to_owned(),
                    )
                })?;
            v.retarget_series(&resolved);
        }
        BumpOperation::Ppa => {
            v.bump_ppa();
        }
        BumpOperation::ClearPpa => {
            v.clear_ppa();
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Internal parsing helpers
// ---------------------------------------------------------------------------

/// Strip a trailing `~ppaN` suffix from the debian revision string.
///
/// Returns `(main_part, ppa_number)`. If no PPA suffix is found, returns
/// the original string and `None`.
fn strip_ppa_suffix(s: &str) -> (&str, Option<u32>) {
    // Look for the last occurrence of "~ppa" followed by digits.
    if let Some(ppa_pos) = s.rfind("~ppa") {
        let after_ppa = &s[ppa_pos + 4..]; // skip "~ppa"
        if !after_ppa.is_empty()
            && after_ppa.chars().all(|c| c.is_ascii_digit())
            && let Ok(n) = after_ppa.parse::<u32>()
        {
            return (&s[..ppa_pos], Some(n));
        }
    }
    (s, None)
}

/// Parse the optional repack number immediately after `+dfsg`.
///
/// Returns `(repack_number, remaining_str)`.
/// If the first character(s) after `+dfsg` are digits, those form the repack
/// number. Otherwise `repack_number` is `None`.
fn parse_repack_number(s: &str) -> (Option<u32>, &str) {
    if s.is_empty() {
        return (None, s);
    }

    // Count leading digits.
    let digit_end = s
        .char_indices()
        .take_while(|(_, c)| c.is_ascii_digit())
        .last()
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);

    if digit_end == 0 {
        return (None, s);
    }

    let num_str = &s[..digit_end];
    let number = num_str.parse::<u32>().ok();
    (number, &s[digit_end..])
}

/// Parse the suffixes after the repack number in the upstream part.
///
/// Expected patterns:
/// - `""` (empty — non-backport)
/// - `"~24.04"` (backport)
/// - `"~24.04.1"` (backport with backport_repack)
/// - `"~24.04~stage0"` (stage0)
/// - `"~24.04.1~stage0"` (stage0 with backport_repack)
fn parse_upstream_suffixes(s: &str) -> Result<(Option<String>, Option<u32>, bool)> {
    if s.is_empty() {
        return Ok((None, None, false));
    }

    // Must start with '~'
    if !s.starts_with('~') {
        return Err(ThermiteError::InvalidRustVersion(format!(
            "unexpected characters after repack number in upstream part: '{s}'"
        )));
    }

    let remainder = &s[1..]; // skip the leading '~'

    // Check for stage0 as the only suffix (no series — shouldn't happen per
    // the spec, but handle gracefully).
    if remainder == "stage0" {
        return Ok((None, None, true));
    }

    // Split on '~' to separate series[.bp_repack] from potential "stage0".
    let (series_part, stage0) = if let Some((before, after)) = remainder.split_once('~') {
        if after == "stage0" {
            (before, true)
        } else {
            // Unknown suffix after second ~; treat the whole thing as series.
            return Err(ThermiteError::InvalidRustVersion(format!(
                "unexpected suffix after series in upstream: '~{after}'"
            )));
        }
    } else {
        (remainder, false)
    };

    // Parse series_part: either "XX.YY" or "XX.YY.N" (backport_repack).
    let (series, backport_repack) = parse_series_and_repack(series_part)?;

    Ok((Some(series), backport_repack, stage0))
}

/// Parse a series string that may include a backport repack suffix.
///
/// Examples:
/// - `"24.04"` → `("24.04", None)`
/// - `"24.04.1"` → `("24.04", Some(1))`
/// - `"20.04.2"` → `("20.04", Some(2))`
fn parse_series_and_repack(s: &str) -> Result<(String, Option<u32>)> {
    // A series number is always XX.YY (two parts). If there's a third dot-separated
    // segment and it's a number, that's the backport_repack.
    let parts: Vec<&str> = s.split('.').collect();
    match parts.len() {
        2 => {
            // Simple series: "XX.YY"
            Ok((s.to_owned(), None))
        }
        3 => {
            // Series with backport_repack: "XX.YY.N"
            let series = format!("{}.{}", parts[0], parts[1]);
            let bp_repack = parts[2].parse::<u32>().map_err(|_| {
                ThermiteError::InvalidRustVersion(format!(
                    "invalid backport repack number in '{s}'"
                ))
            })?;
            Ok((series, Some(bp_repack)))
        }
        _ => Err(ThermiteError::InvalidRustVersion(format!(
            "invalid series format: '{s}'"
        ))),
    }
}

/// Parse the debian revision part.
///
/// Expected formats:
/// - `"0ubuntu<N>"` (non-backport)
/// - `"0ubuntu<N>~<series>.<backport_revision>"` (backport)
fn parse_debian_revision(rev: &str, full_version: &str) -> Result<(u32, Option<u32>)> {
    // Must start with "0ubuntu"
    let after_0ubuntu = rev.strip_prefix("0ubuntu").ok_or_else(|| {
        ThermiteError::InvalidRustVersion(format!(
            "debian revision must start with '0ubuntu': got '{rev}' in '{full_version}'"
        ))
    })?;

    // Split on '~' to find the optional backport suffix.
    if let Some((revision_str, backport_suffix)) = after_0ubuntu.split_once('~') {
        let ubuntu_rev = revision_str.parse::<u32>().map_err(|_| {
            ThermiteError::InvalidRustVersion(format!(
                "invalid ubuntu revision number '{revision_str}' in '{full_version}'"
            ))
        })?;

        // backport_suffix should be "<series>.<backport_revision>"
        // The series is XX.YY and the backport_revision is the last segment.
        let bp_rev = parse_backport_revision_from_suffix(backport_suffix, full_version)?;

        Ok((ubuntu_rev, Some(bp_rev)))
    } else {
        // No '~' — plain ubuntu revision.
        let ubuntu_rev = after_0ubuntu.parse::<u32>().map_err(|_| {
            ThermiteError::InvalidRustVersion(format!(
                "invalid ubuntu revision number '{after_0ubuntu}' in '{full_version}'"
            ))
        })?;
        Ok((ubuntu_rev, None))
    }
}

/// Parse the backport revision from the suffix after `~` in the debian revision.
///
/// The suffix has the form `<series>.<backport_revision>` where series is `XX.YY`.
/// So the full suffix looks like `XX.YY.N` — the last dot-separated segment is
/// the backport revision.
fn parse_backport_revision_from_suffix(suffix: &str, full_version: &str) -> Result<u32> {
    // The suffix is "XX.YY.N" — we want the last segment.
    let last_dot = suffix.rfind('.').ok_or_else(|| {
        ThermiteError::InvalidRustVersion(format!(
            "expected '<series>.<backport_revision>' but got '{suffix}' in '{full_version}'"
        ))
    })?;

    let bp_rev_str = &suffix[last_dot + 1..];
    bp_rev_str.parse::<u32>().map_err(|_| {
        ThermiteError::InvalidRustVersion(format!(
            "invalid backport revision '{bp_rev_str}' in '{full_version}'"
        ))
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- Parsing tests ---

    #[test]
    fn parse_simple_non_backport() {
        let v = RustcPackageVersion::parse("1.85.0+dfsg-0ubuntu1").unwrap();
        assert_eq!(v.upstream, RustVersion::parse("1.85.0").unwrap());
        assert_eq!(v.repack_number, None);
        assert_eq!(v.series, None);
        assert_eq!(v.backport_repack, None);
        assert!(!v.stage0);
        assert_eq!(v.ubuntu_revision, 1);
        assert_eq!(v.backport_revision, None);
    }

    #[test]
    fn parse_non_backport_with_repack() {
        let v = RustcPackageVersion::parse("1.85.0+dfsg3-0ubuntu5").unwrap();
        assert_eq!(v.upstream, RustVersion::parse("1.85.0").unwrap());
        assert_eq!(v.repack_number, Some(3));
        assert_eq!(v.series, None);
        assert_eq!(v.backport_repack, None);
        assert!(!v.stage0);
        assert_eq!(v.ubuntu_revision, 5);
        assert_eq!(v.backport_revision, None);
    }

    #[test]
    fn parse_backport_simple() {
        let v = RustcPackageVersion::parse("1.90.0+dfsg2~24.04-0ubuntu3~24.04.1").unwrap();
        assert_eq!(v.upstream, RustVersion::parse("1.90.0").unwrap());
        assert_eq!(v.repack_number, Some(2));
        assert_eq!(v.series, Some("24.04".to_owned()));
        assert_eq!(v.backport_repack, None);
        assert!(!v.stage0);
        assert_eq!(v.ubuntu_revision, 3);
        assert_eq!(v.backport_revision, Some(1));
    }

    #[test]
    fn parse_backport_with_backport_repack() {
        let v = RustcPackageVersion::parse("1.90.0+dfsg2~24.04.1-0ubuntu3~24.04.1").unwrap();
        assert_eq!(v.upstream, RustVersion::parse("1.90.0").unwrap());
        assert_eq!(v.repack_number, Some(2));
        assert_eq!(v.series, Some("24.04".to_owned()));
        assert_eq!(v.backport_repack, Some(1));
        assert!(!v.stage0);
        assert_eq!(v.ubuntu_revision, 3);
        assert_eq!(v.backport_revision, Some(1));
    }

    #[test]
    fn parse_backport_with_higher_backport_revision() {
        let v = RustcPackageVersion::parse("1.90.0+dfsg2~22.04-0ubuntu3~22.04.2").unwrap();
        assert_eq!(v.series, Some("22.04".to_owned()));
        assert_eq!(v.backport_revision, Some(2));
    }

    #[test]
    fn parse_stage0() {
        let v = RustcPackageVersion::parse("1.92.0+dfsg~24.04~stage0-0ubuntu1~24.04.3").unwrap();
        assert_eq!(v.upstream, RustVersion::parse("1.92.0").unwrap());
        assert_eq!(v.repack_number, None);
        assert_eq!(v.series, Some("24.04".to_owned()));
        assert_eq!(v.backport_repack, None);
        assert!(v.stage0);
        assert_eq!(v.ubuntu_revision, 1);
        assert_eq!(v.backport_revision, Some(3));
    }

    #[test]
    fn parse_bare_dfsg_backport() {
        // No repack number, but has backport series.
        let v = RustcPackageVersion::parse("1.93.0+dfsg~24.04-0ubuntu1~24.04.1").unwrap();
        assert_eq!(v.upstream, RustVersion::parse("1.93.0").unwrap());
        assert_eq!(v.repack_number, None);
        assert_eq!(v.series, Some("24.04".to_owned()));
        assert_eq!(v.backport_repack, None);
        assert_eq!(v.ubuntu_revision, 1);
        assert_eq!(v.backport_revision, Some(1));
    }

    // --- Round-trip tests (parse -> Display -> parse) ---

    #[test]
    fn roundtrip_non_backport() {
        let input = "1.95.0+dfsg-0ubuntu1";
        let v = RustcPackageVersion::parse(input).unwrap();
        assert_eq!(v.to_string(), input);
    }

    #[test]
    fn roundtrip_non_backport_with_repack() {
        let input = "1.85.0+dfsg3-0ubuntu5";
        let v = RustcPackageVersion::parse(input).unwrap();
        assert_eq!(v.to_string(), input);
    }

    #[test]
    fn roundtrip_backport() {
        let input = "1.90.0+dfsg2~24.04-0ubuntu3~24.04.1";
        let v = RustcPackageVersion::parse(input).unwrap();
        assert_eq!(v.to_string(), input);
    }

    #[test]
    fn roundtrip_backport_with_repack() {
        let input = "1.90.0+dfsg2~24.04.1-0ubuntu3~24.04.1";
        let v = RustcPackageVersion::parse(input).unwrap();
        assert_eq!(v.to_string(), input);
    }

    #[test]
    fn roundtrip_stage0() {
        let input = "1.92.0+dfsg~24.04~stage0-0ubuntu1~24.04.3";
        let v = RustcPackageVersion::parse(input).unwrap();
        assert_eq!(v.to_string(), input);
    }

    // --- Display tests ---

    #[test]
    fn display_non_backport() {
        let v = RustcPackageVersion::new(
            RustVersion::parse("1.95.0").unwrap(),
            None,
            None,
            None,
            false,
            1,
            None,
        );
        assert_eq!(v.to_string(), "1.95.0+dfsg-0ubuntu1");
    }

    #[test]
    fn display_backport() {
        let v = RustcPackageVersion::new(
            RustVersion::parse("1.90.0").unwrap(),
            Some(2),
            Some("24.04".to_owned()),
            None,
            false,
            3,
            Some(1),
        );
        assert_eq!(v.to_string(), "1.90.0+dfsg2~24.04-0ubuntu3~24.04.1");
    }

    #[test]
    fn display_stage0() {
        let v = RustcPackageVersion::new(
            RustVersion::parse("1.92.0").unwrap(),
            None,
            Some("24.04".to_owned()),
            None,
            true,
            1,
            Some(3),
        );
        assert_eq!(v.to_string(), "1.92.0+dfsg~24.04~stage0-0ubuntu1~24.04.3");
    }

    // --- Bump method tests ---

    #[test]
    fn bump_ubuntu_revision() {
        let mut v = RustcPackageVersion::parse("1.95.0+dfsg-0ubuntu2").unwrap();
        v.bump_ubuntu_revision();
        assert_eq!(v.to_string(), "1.95.0+dfsg-0ubuntu3");
    }

    #[test]
    fn bump_repack_from_none() {
        let mut v = RustcPackageVersion::parse("1.95.0+dfsg-0ubuntu3").unwrap();
        v.bump_repack();
        assert_eq!(v.to_string(), "1.95.0+dfsg1-0ubuntu1");
    }

    #[test]
    fn bump_repack_increments() {
        let mut v = RustcPackageVersion::parse("1.95.0+dfsg1-0ubuntu2").unwrap();
        v.bump_repack();
        assert_eq!(v.to_string(), "1.95.0+dfsg2-0ubuntu1");
    }

    #[test]
    fn bump_patch_release() {
        let mut v = RustcPackageVersion::parse("1.95.0+dfsg3-0ubuntu5").unwrap();
        v.bump_patch_release(RustVersion::parse("1.95.1").unwrap());
        assert_eq!(v.to_string(), "1.95.1+dfsg-0ubuntu1");
    }

    #[test]
    fn to_backport_from_non_backport() {
        let mut v = RustcPackageVersion::parse("1.95.0+dfsg-0ubuntu2").unwrap();
        v.to_backport("24.04");
        assert_eq!(v.to_string(), "1.95.0+dfsg~24.04-0ubuntu2~24.04.1");
    }

    #[test]
    fn to_backport_with_repack() {
        let mut v = RustcPackageVersion::parse("1.90.0+dfsg2-0ubuntu3").unwrap();
        v.to_backport("24.04");
        assert_eq!(v.to_string(), "1.90.0+dfsg2~24.04-0ubuntu3~24.04.1");
    }

    #[test]
    fn retarget_series() {
        let mut v = RustcPackageVersion::parse("1.90.0+dfsg2~24.04-0ubuntu3~24.04.1").unwrap();
        v.retarget_series("22.04");
        assert_eq!(v.to_string(), "1.90.0+dfsg2~22.04-0ubuntu3~22.04.1");
    }

    #[test]
    fn retarget_series_resets_backport_repack() {
        let mut v = RustcPackageVersion::parse("1.90.0+dfsg2~24.04.1-0ubuntu3~24.04.2").unwrap();
        v.retarget_series("22.04");
        assert_eq!(v.to_string(), "1.90.0+dfsg2~22.04-0ubuntu3~22.04.1");
    }

    #[test]
    fn bump_backport_revision() {
        let mut v = RustcPackageVersion::parse("1.90.0+dfsg2~24.04-0ubuntu3~24.04.1").unwrap();
        v.bump_backport_revision();
        assert_eq!(v.to_string(), "1.90.0+dfsg2~24.04-0ubuntu3~24.04.2");
    }

    #[test]
    fn bump_backport_repack() {
        let mut v = RustcPackageVersion::parse("1.90.0+dfsg2~24.04-0ubuntu3~24.04.2").unwrap();
        v.bump_backport_repack();
        assert_eq!(v.to_string(), "1.90.0+dfsg2~24.04.1-0ubuntu3~24.04.1");
    }

    #[test]
    fn bump_backport_repack_increments() {
        let mut v = RustcPackageVersion::parse("1.90.0+dfsg2~24.04.1-0ubuntu3~24.04.1").unwrap();
        v.bump_backport_repack();
        assert_eq!(v.to_string(), "1.90.0+dfsg2~24.04.2-0ubuntu3~24.04.1");
    }

    // --- Error case tests ---

    #[test]
    fn parse_missing_hyphen() {
        assert!(RustcPackageVersion::parse("1.85.0+dfsg0ubuntu1").is_err());
    }

    #[test]
    fn parse_missing_dfsg() {
        assert!(RustcPackageVersion::parse("1.85.0-0ubuntu1").is_err());
    }

    #[test]
    fn parse_invalid_upstream_version() {
        assert!(RustcPackageVersion::parse("1.85+dfsg-0ubuntu1").is_err());
    }

    #[test]
    fn parse_missing_0ubuntu() {
        assert!(RustcPackageVersion::parse("1.85.0+dfsg-ubuntu1").is_err());
    }

    #[test]
    fn parse_non_numeric_ubuntu_revision() {
        assert!(RustcPackageVersion::parse("1.85.0+dfsg-0ubuntux").is_err());
    }

    // --- Doc examples from version-strings.md ---

    #[test]
    fn doc_example_initial_upload() {
        let v = RustcPackageVersion::parse("1.95.0+dfsg-0ubuntu1").unwrap();
        assert_eq!(v.upstream, RustVersion::parse("1.95.0").unwrap());
        assert_eq!(v.repack_number, None);
        assert_eq!(v.ubuntu_revision, 1);
        assert!(!v.is_backport());
    }

    #[test]
    fn doc_example_fixing_a_bug() {
        // "1.95.0+dfsg-0ubuntu1" → increment ubuntu_revision → "1.95.0+dfsg-0ubuntu2"
        let mut v = RustcPackageVersion::parse("1.95.0+dfsg-0ubuntu1").unwrap();
        v.bump_ubuntu_revision();
        assert_eq!(v.to_string(), "1.95.0+dfsg-0ubuntu2");
    }

    #[test]
    fn doc_example_pruning_repack() {
        // "1.95.0+dfsg-0ubuntu2" → bump repack → "1.95.0+dfsg1-0ubuntu1"
        let mut v = RustcPackageVersion::parse("1.95.0+dfsg-0ubuntu2").unwrap();
        v.bump_repack();
        assert_eq!(v.to_string(), "1.95.0+dfsg1-0ubuntu1");
    }

    #[test]
    fn doc_example_patch_release() {
        // "1.95.0+dfsg1-0ubuntu1" → new upstream 1.95.1 → "1.95.1+dfsg-0ubuntu1"
        let mut v = RustcPackageVersion::parse("1.95.0+dfsg1-0ubuntu1").unwrap();
        v.bump_patch_release(RustVersion::parse("1.95.1").unwrap());
        assert_eq!(v.to_string(), "1.95.1+dfsg-0ubuntu1");
    }

    #[test]
    fn doc_example_backport_to_2404() {
        // "1.90.0+dfsg2-0ubuntu3" → backport to 24.04
        // → "1.90.0+dfsg2~24.04-0ubuntu3~24.04.1"
        let mut v = RustcPackageVersion::parse("1.90.0+dfsg2-0ubuntu3").unwrap();
        v.to_backport("24.04");
        assert_eq!(v.to_string(), "1.90.0+dfsg2~24.04-0ubuntu3~24.04.1");
    }

    #[test]
    fn doc_example_backport_fix_bug() {
        // "1.90.0+dfsg2~24.04-0ubuntu3~24.04.1" → bump backport_revision
        // → "1.90.0+dfsg2~24.04-0ubuntu3~24.04.2"
        let mut v = RustcPackageVersion::parse("1.90.0+dfsg2~24.04-0ubuntu3~24.04.1").unwrap();
        v.bump_backport_revision();
        assert_eq!(v.to_string(), "1.90.0+dfsg2~24.04-0ubuntu3~24.04.2");
    }

    #[test]
    fn doc_example_backport_repack_issue() {
        // "1.90.0+dfsg2~24.04-0ubuntu3~24.04.2" → bump backport_repack
        // → "1.90.0+dfsg2~24.04.1-0ubuntu3~24.04.1"
        let mut v = RustcPackageVersion::parse("1.90.0+dfsg2~24.04-0ubuntu3~24.04.2").unwrap();
        v.bump_backport_repack();
        assert_eq!(v.to_string(), "1.90.0+dfsg2~24.04.1-0ubuntu3~24.04.1");
    }

    #[test]
    fn doc_example_backport_to_2204() {
        // "1.90.0+dfsg2~24.04-0ubuntu3~24.04.1" → retarget to 22.04
        // → "1.90.0+dfsg2~22.04-0ubuntu3~22.04.1"
        let mut v = RustcPackageVersion::parse("1.90.0+dfsg2~24.04-0ubuntu3~24.04.1").unwrap();
        v.retarget_series("22.04");
        assert_eq!(v.to_string(), "1.90.0+dfsg2~22.04-0ubuntu3~22.04.1");
    }

    // --- is_backport / is_stage0 ---

    #[test]
    fn is_backport_detection() {
        let v = RustcPackageVersion::parse("1.95.0+dfsg-0ubuntu1").unwrap();
        assert!(!v.is_backport());

        let v = RustcPackageVersion::parse("1.90.0+dfsg2~24.04-0ubuntu3~24.04.1").unwrap();
        assert!(v.is_backport());
    }

    #[test]
    fn is_stage0_detection() {
        let v = RustcPackageVersion::parse("1.95.0+dfsg-0ubuntu1").unwrap();
        assert!(!v.is_stage0());

        let v = RustcPackageVersion::parse("1.92.0+dfsg~24.04~stage0-0ubuntu1~24.04.3").unwrap();
        assert!(v.is_stage0());
    }

    // --- PPA suffix tests ---

    #[test]
    fn parse_non_backport_with_ppa() {
        let v = RustcPackageVersion::parse("1.88.0+dfsg-0ubuntu1~ppa1").unwrap();
        assert_eq!(v.upstream, RustVersion::parse("1.88.0").unwrap());
        assert_eq!(v.repack_number, None);
        assert_eq!(v.series, None);
        assert_eq!(v.ubuntu_revision, 1);
        assert_eq!(v.ppa, Some(1));
    }

    #[test]
    fn parse_backport_with_ppa() {
        let v = RustcPackageVersion::parse("1.92.0+dfsg~20.04-0ubuntu1~20.04.1~ppa1").unwrap();
        assert_eq!(v.upstream, RustVersion::parse("1.92.0").unwrap());
        assert_eq!(v.series, Some("20.04".to_owned()));
        assert_eq!(v.backport_revision, Some(1));
        assert_eq!(v.ppa, Some(1));
    }

    #[test]
    fn roundtrip_with_ppa() {
        let input = "1.88.0+dfsg-0ubuntu1~ppa3";
        let v = RustcPackageVersion::parse(input).unwrap();
        assert_eq!(v.to_string(), input);
    }

    #[test]
    fn roundtrip_backport_with_ppa() {
        let input = "1.92.0+dfsg~20.04-0ubuntu1~20.04.1~ppa1";
        let v = RustcPackageVersion::parse(input).unwrap();
        assert_eq!(v.to_string(), input);
    }

    #[test]
    fn bump_ppa() {
        let mut v = RustcPackageVersion::parse("1.88.0+dfsg-0ubuntu1~ppa1").unwrap();
        v.bump_ppa();
        assert_eq!(v.to_string(), "1.88.0+dfsg-0ubuntu1~ppa2");
    }

    #[test]
    fn bump_ppa_from_none() {
        let mut v = RustcPackageVersion::parse("1.88.0+dfsg-0ubuntu1").unwrap();
        v.bump_ppa();
        assert_eq!(v.to_string(), "1.88.0+dfsg-0ubuntu1~ppa1");
    }

    #[test]
    fn clear_ppa() {
        let mut v = RustcPackageVersion::parse("1.88.0+dfsg-0ubuntu1~ppa3").unwrap();
        v.clear_ppa();
        assert_eq!(v.to_string(), "1.88.0+dfsg-0ubuntu1");
    }

    #[test]
    fn to_backport_clears_ppa() {
        let mut v = RustcPackageVersion::parse("1.95.0+dfsg-0ubuntu2~ppa1").unwrap();
        v.to_backport("24.04");
        assert_eq!(v.to_string(), "1.95.0+dfsg~24.04-0ubuntu2~24.04.1");
        assert_eq!(v.ppa, None);
    }

    #[test]
    fn ppa_not_serialized_when_none() {
        let v = RustcPackageVersion::parse("1.95.0+dfsg-0ubuntu1").unwrap();
        let json = serde_json::to_string(&v).unwrap();
        assert!(!json.contains("ppa"));
    }

    #[test]
    fn ppa_serialized_when_present() {
        let v = RustcPackageVersion::parse("1.88.0+dfsg-0ubuntu1~ppa3").unwrap();
        let json = serde_json::to_string(&v).unwrap();
        assert!(json.contains("\"ppa\":3"));
    }
}
