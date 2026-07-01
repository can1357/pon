use std::collections::BTreeSet;
use std::fmt;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Tag {
    pub python: String,
    pub abi: String,
    pub platform: String,
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
}
