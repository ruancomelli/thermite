use crate::error::{Result, ThermiteError};

/// The set of known Ubuntu release adjectives paired with their series numbers.
///
/// This list covers all Ubuntu releases from Focal onwards. Add new entries
/// here as new Ubuntu releases are announced.
const KNOWN_RELEASES: &[(&str, &str)] = &[
    ("focal", "20.04"),    // 20.04 LTS
    ("groovy", "20.10"),   // 20.10
    ("hirsute", "21.04"),  // 21.04
    ("impish", "21.10"),   // 21.10
    ("jammy", "22.04"),    // 22.04 LTS
    ("kinetic", "22.10"),  // 22.10
    ("lunar", "23.04"),    // 23.04
    ("mantic", "23.10"),   // 23.10
    ("noble", "24.04"),    // 24.04 LTS
    ("oracular", "24.10"), // 24.10
    ("plucky", "25.04"),   // 25.04
    ("questing", "25.10"), // 25.10
    ("resolute", "26.04"), // 26.04 LTS
    ("stonking", "26.10"), // 26.10
];

/// A validated Ubuntu release adjective (e.g. `"noble"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UbuntuRelease(String);

impl UbuntuRelease {
    /// Parse and validate a release adjective string.
    pub fn parse(s: &str) -> Result<Self> {
        if KNOWN_RELEASES.iter().any(|(name, _)| *name == s) {
            Ok(Self(s.to_owned()))
        } else {
            Err(ThermiteError::UnknownRelease(s.to_owned()))
        }
    }

    /// Return the release adjective as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Return the numeric series identifier for this release (e.g. `"22.04"`).
    ///
    /// Panics if the release was somehow constructed with an unknown adjective,
    /// which cannot happen through the public [`Self::parse`] constructor.
    pub fn series_number(&self) -> &'static str {
        KNOWN_RELEASES
            .iter()
            .find(|(name, _)| *name == self.0.as_str())
            .map(|(_, series)| *series)
            .expect("UbuntuRelease invariant: adjective is always in KNOWN_RELEASES")
    }

    /// Return the adjective of the current Ubuntu development release.
    ///
    /// The devel release is defined as the **last** entry in `KNOWN_RELEASES`.
    /// This list is appended to as new Ubuntu releases are announced, so the
    /// value returned here tracks the current devel release as long as the
    /// list is kept up to date.
    pub fn devel() -> &'static str {
        KNOWN_RELEASES
            .last()
            .map(|(name, _)| *name)
            .expect("KNOWN_RELEASES is non-empty by construction")
    }

    /// Construct the [`UbuntuRelease`] for the current devel release.
    ///
    /// Equivalent to `UbuntuRelease::parse(UbuntuRelease::devel()).unwrap()`
    /// but infallible, since [`devel`](Self::devel) always returns a known
    /// adjective.
    pub fn devel_release() -> Self {
        Self(Self::devel().to_owned())
    }

    /// Return `true` when this release is the current Ubuntu development
    /// release (i.e. the last entry in `KNOWN_RELEASES`).
    ///
    /// This is a **static** heuristic: it is correct only as long as
    /// `KNOWN_RELEASES` is kept up to date with the release cadence. Callers
    /// that need to be robust against the devel→stable transition should treat
    /// this as a hint for messaging only, not as the source of truth for
    /// branch-name decisions.
    pub fn is_devel(&self) -> bool {
        self.0 == Self::devel()
    }

    /// Return `true` when this release is an Ubuntu Long-Term Support release.
    ///
    /// LTS releases fall on even-numbered years in April (`YY.04`). This is
    /// derived from [`series_number()`](Self::series_number) rather than a
    /// static list so that it stays correct as new LTS releases are appended
    /// to `KNOWN_RELEASES`.
    pub fn is_lts(&self) -> bool {
        let series = self.series_number();
        let (year, month) = series
            .split_once('.')
            .expect("series_number always has the form YY.MM");
        let year: u32 = year.parse().expect("year component is numeric");
        year.is_multiple_of(2) && month == "04"
    }

    /// Return the ordered chain of releases that backports traverse, from the
    /// current devel release down to the oldest supported LTS.
    ///
    /// The chain consists of the current devel release (the last entry in
    /// `KNOWN_RELEASES`) followed by every LTS release in `KNOWN_RELEASES`,
    /// newest-first. Non-LTS, non-devel releases are excluded — backports
    /// normally go one LTS at a time.
    ///
    /// Example (today): `["stonking", "resolute", "noble", "jammy", "focal"]`.
    pub fn backport_chain() -> Vec<&'static str> {
        let mut chain: Vec<&'static str> = KNOWN_RELEASES
            .iter()
            .rev()
            .filter(|(name, series)| *name == Self::devel() || Self::series_is_lts(series))
            .map(|(name, _)| *name)
            .collect();
        // `devel()` may itself be an LTS release (e.g. a 26.04 devel before
        // release); dedup so it appears only once, at the head.
        chain.dedup();
        chain
    }

    /// Return the position of this release in [`Self::backport_chain()`], or
    /// `None` when the release is not part of the LTS+devel chain (i.e. it is
    /// a non-LTS, non-devel release such as `oracular` or `questing`).
    pub fn chain_position(&self) -> Option<usize> {
        Self::backport_chain()
            .iter()
            .position(|name| *name == self.0.as_str())
    }

    /// Return `true` when `series` (a `"YY.MM"` string) is an LTS series.
    ///
    /// Helper for [`Self::backport_chain`] / [`Self::is_lts`] so the LTS test
    /// lives in one place.
    fn series_is_lts(series: &str) -> bool {
        let Some((year, month)) = series.split_once('.') else {
            return false;
        };
        let Ok(year) = year.parse::<u32>() else {
            return false;
        };
        year.is_multiple_of(2) && month == "04"
    }
}

impl std::fmt::Display for UbuntuRelease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_known_release() {
        assert!(UbuntuRelease::parse("noble").is_ok());
        assert!(UbuntuRelease::parse("jammy").is_ok());
    }

    #[test]
    fn reject_unknown_release() {
        assert!(UbuntuRelease::parse("foobar").is_err());
        assert!(UbuntuRelease::parse("").is_err());
    }

    #[test]
    fn series_number_returns_correct_value() {
        assert_eq!(
            UbuntuRelease::parse("jammy").unwrap().series_number(),
            "22.04"
        );
        assert_eq!(
            UbuntuRelease::parse("noble").unwrap().series_number(),
            "24.04"
        );
        assert_eq!(
            UbuntuRelease::parse("focal").unwrap().series_number(),
            "20.04"
        );
        assert_eq!(
            UbuntuRelease::parse("questing").unwrap().series_number(),
            "25.10"
        );
    }

    #[test]
    fn devel_returns_last_known_release_adjective() {
        let last = KNOWN_RELEASES.last().expect("KNOWN_RELEASES is non-empty");
        assert_eq!(UbuntuRelease::devel(), last.0);
    }

    #[test]
    fn is_devel_true_for_last_known_release() {
        let last = KNOWN_RELEASES.last().expect("KNOWN_RELEASES is non-empty");
        let r = UbuntuRelease::parse(last.0).unwrap();
        assert!(r.is_devel(), "{} should be devel", last.0);
    }

    #[test]
    fn is_devel_false_for_non_last_release() {
        let r = UbuntuRelease::parse("noble").unwrap();
        // `noble` is only the devel release if it happens to be the last entry
        // in KNOWN_RELEASES; assert against the known devel adjective instead.
        if UbuntuRelease::devel() != "noble" {
            assert!(!r.is_devel());
        }
    }

    #[test]
    fn devel_release_matches_parse_of_devel_adjective() {
        let via_devel_release = UbuntuRelease::devel_release();
        let via_parse = UbuntuRelease::parse(UbuntuRelease::devel()).unwrap();
        assert_eq!(via_devel_release.as_str(), via_parse.as_str());
        assert!(via_devel_release.is_devel());
    }

    #[test]
    fn is_lts_true_for_known_lts_releases() {
        for name in ["focal", "jammy", "noble", "resolute"] {
            let r = UbuntuRelease::parse(name).unwrap();
            assert!(r.is_lts(), "{name} should be LTS");
        }
    }

    #[test]
    fn is_lts_false_for_non_lts_releases() {
        for name in ["groovy", "oracular", "questing", "stonking"] {
            let r = UbuntuRelease::parse(name).unwrap();
            // `stonking` (26.10) is the current devel; if a future LTS is
            // appended it stays non-LTS. Guard against the devel being LTS.
            if !r.is_devel() {
                assert!(!r.is_lts(), "{name} should not be LTS");
            }
        }
    }

    #[test]
    fn backport_chain_starts_with_devel_then_lts_newest_first() {
        let chain = UbuntuRelease::backport_chain();
        assert!(!chain.is_empty(), "chain should be non-empty");
        // Head is always the current devel release.
        assert_eq!(chain[0], UbuntuRelease::devel());
        // Every subsequent entry is an LTS release, newest-first.
        for name in &chain[1..] {
            let r = UbuntuRelease::parse(name).unwrap();
            assert!(r.is_lts(), "{name} in chain should be LTS");
        }
        // LTS entries are in descending series-number order.
        let lts_series: Vec<&str> = chain[1..]
            .iter()
            .map(|name| UbuntuRelease::parse(name).unwrap().series_number())
            .collect();
        let mut sorted = lts_series.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(lts_series, sorted, "LTS entries should be newest-first");
    }

    #[test]
    fn chain_position_returns_index_for_chain_members() {
        let chain = UbuntuRelease::backport_chain();
        for (idx, name) in chain.iter().enumerate() {
            let r = UbuntuRelease::parse(name).unwrap();
            assert_eq!(
                r.chain_position(),
                Some(idx),
                "{name} should be at position {idx}"
            );
        }
    }

    #[test]
    fn chain_position_none_for_non_chain_release() {
        // `oracular` is non-LTS and non-devel, so it is not in the chain.
        let r = UbuntuRelease::parse("oracular").unwrap();
        assert_eq!(r.chain_position(), None);
    }

    #[test]
    fn backport_chain_today_matches_expected() {
        // Snapshot test for the current KNOWN_RELEASES. Update when new
        // releases are appended.
        assert_eq!(
            UbuntuRelease::backport_chain(),
            vec!["stonking", "resolute", "noble", "jammy", "focal"]
        );
    }
}
