use pep440_rs::{Version, VersionSpecifiers};
use version_ranges::Ranges;

/// PubGrub range for `spec` over the enumerated candidate set.
///
/// Membership is decided by `VersionSpecifiers::contains`; the returned range is
/// the union of singleton ranges for candidates accepted by the PEP 440
/// prerelease policy.
#[must_use]
pub fn range_from_specifiers(
    spec: &VersionSpecifiers,
    candidates: &[Version],
    allow_prerelease: bool,
) -> Ranges<Version> {
    let mentions_prerelease = spec_mentions_prerelease(spec);
    let all_candidates_are_prereleases = candidates.iter().all(Version::any_prerelease);

    candidates
        .iter()
        .filter(|candidate| {
            spec.contains(candidate)
                && (allow_prerelease
                    || mentions_prerelease
                    || candidate.is_stable()
                    || all_candidates_are_prereleases)
        })
        .fold(Ranges::empty(), |ranges, candidate| {
            ranges.union(&Ranges::singleton(candidate.clone()))
        })
}

fn spec_mentions_prerelease(spec: &VersionSpecifiers) -> bool {
    spec.iter().any(|specifier| specifier.any_prerelease())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    fn version(raw: &str) -> Version {
        Version::from_str(raw).expect("version")
    }

    #[test]
    fn range_uses_specifiers_as_membership_oracle() {
        let spec = VersionSpecifiers::from_str(">=1.0,!=1.5,<2.0").expect("spec");
        let candidates = [version("1.0"), version("1.5"), version("2.0")];
        let range = range_from_specifiers(&spec, &candidates, false);

        assert!(range.contains(&version("1.0")));
        assert!(!range.contains(&version("1.5")));
        assert!(!range.contains(&version("2.0")));
    }

    #[test]
    fn prereleases_require_flag_mention_or_all_prerelease_candidates() {
        let spec = VersionSpecifiers::from_str(">=1.0").expect("spec");
        let mixed = [version("1.1a1"), version("1.1")];

        assert!(!range_from_specifiers(&spec, &mixed, false).contains(&version("1.1a1")));
        assert!(range_from_specifiers(&spec, &mixed, true).contains(&version("1.1a1")));

        let mentioned = VersionSpecifiers::from_str(">=1.0a1").expect("mentioned");
        assert!(range_from_specifiers(&mentioned, &mixed, false).contains(&version("1.1a1")));

        let all_prerelease = [version("1.1a1"), version("2.0a1")];
        assert!(range_from_specifiers(&spec, &all_prerelease, false).contains(&version("1.1a1")));
    }
}
