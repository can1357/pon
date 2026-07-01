use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Tag {
    pub python: String,
    pub abi: String,
    pub platform: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WheelCompatibility {
    PurePython,
    CAbiRefused { reason: String },
}

impl Tag {
    #[must_use]
    pub fn new(python: impl Into<String>, abi: impl Into<String>, platform: impl Into<String>) -> Self {
        Self {
            python: python.into(),
            abi: abi.into(),
            platform: platform.into(),
        }
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}-{}", self.python, self.abi, self.platform)
    }
}

#[must_use]
pub fn parse_tag_set(python: &str, abi: &str, platform: &str) -> Vec<Tag> {
    let mut tags = Vec::new();
    for py in python.split('.') {
        for abi in abi.split('.') {
            for platform in platform.split('.') {
                tags.push(Tag::new(py, abi, platform));
            }
        }
    }
    tags
}

#[must_use]
pub fn default_supported_tags() -> BTreeSet<Tag> {
    let mut tags = BTreeSet::new();
    tags.insert(Tag::new("py3", "none", "any"));
    tags.insert(Tag::new("py2", "none", "any"));
    tags.insert(Tag::new("py2.py3", "none", "any"));
    tags
}

#[must_use]
pub fn any_supported(candidate: &[Tag], supported: &BTreeSet<Tag>) -> bool {
    candidate.iter().any(|tag| supported.contains(tag))
}

#[must_use]
pub fn classify_tags(candidate: &[Tag], supported: &BTreeSet<Tag>) -> WheelCompatibility {
    if any_supported(candidate, supported) {
        WheelCompatibility::PurePython
    } else {
        let candidate_tags = candidate.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ");
        WheelCompatibility::CAbiRefused {
            reason: format!(
                "Pon can install pure Python py*-none-any wheels only; candidate tags `{candidate_tags}` target a C ABI or platform wheel"
            ),
        }
    }
}

#[must_use]
pub fn classify_root_is_purelib(metadata: &str) -> WheelCompatibility {
    for line in metadata.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("Root-Is-Purelib") {
            return if value.trim().eq_ignore_ascii_case("true") {
                WheelCompatibility::PurePython
            } else {
                WheelCompatibility::CAbiRefused {
                    reason: "wheel metadata Root-Is-Purelib is not true".to_owned(),
                }
            };
        }
    }
    WheelCompatibility::CAbiRefused {
        reason: "wheel metadata omits Root-Is-Purelib".to_owned(),
    }
}

#[must_use]
pub fn classify_archive_member(path: &str) -> WheelCompatibility {
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("so" | "pyd" | "dylib") => WheelCompatibility::CAbiRefused {
            reason: format!("wheel archive contains native extension member `{path}`"),
        },
        _ => WheelCompatibility::PurePython,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_compressed_tag_sets() {
        let tags = parse_tag_set("py2.py3", "none", "any");
        assert_eq!(tags, vec![Tag::new("py2", "none", "any"), Tag::new("py3", "none", "any")]);
    }

    #[test]
    fn detects_supported_pure_python_tag() {
        let tags = parse_tag_set("py3", "none", "any");
        assert!(any_supported(&tags, &default_supported_tags()));
        let tags = parse_tag_set("cp312", "abi3", "macosx_14_0_arm64");
        assert!(!any_supported(&tags, &default_supported_tags()));
    }

    #[test]
    fn classifies_root_is_purelib_metadata() {
        assert_eq!(
            classify_root_is_purelib("Wheel-Version: 1.0\nRoot-Is-Purelib: true\n"),
            WheelCompatibility::PurePython
        );
        assert!(matches!(
            classify_root_is_purelib("Wheel-Version: 1.0\nRoot-Is-Purelib: false\n"),
            WheelCompatibility::CAbiRefused { .. }
        ));
    }

    #[test]
    fn classifies_native_archive_members() {
        assert_eq!(classify_archive_member("pkg/__init__.py"), WheelCompatibility::PurePython);
        assert!(matches!(
            classify_archive_member("pkg/_speedups.cpython-314-darwin.so"),
            WheelCompatibility::CAbiRefused { .. }
        ));
    }
}
