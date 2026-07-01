use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::names;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackageKind {
    Pure,
    Native,
    CAbiRefused { reason: String },
}

impl PackageKind {
    #[must_use]
    pub fn lock_kind(&self) -> Option<&'static str> {
        match self {
            Self::Pure => None,
            Self::Native => Some("native"),
            Self::CAbiRefused { .. } => Some("cabi-refused"),
        }
    }

    #[must_use]
    pub fn is_refused(&self) -> bool {
        matches!(self, Self::CAbiRefused { .. })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageRecord {
    pub name: String,
    pub version: String,
    pub kind: PackageKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackageSource {
    Registry { name: String, index_url: Option<String> },
    Url(String),
    Path(PathBuf),
}

impl PackageSource {
    pub fn parse(input: impl AsRef<str>) -> Result<Self> {
        let input = input.as_ref().trim();
        if input.is_empty() {
            return Err(Error::InvalidRequirement(input.to_owned()));
        }
        if input.starts_with("http://") || input.starts_with("https://") || input.starts_with("file://") {
            return Ok(Self::Url(input.to_owned()));
        }
        if input.starts_with('.') || input.starts_with('/') || input.contains(std::path::MAIN_SEPARATOR) {
            return Ok(Self::Path(PathBuf::from(input)));
        }
        names::validate(input)?;
        Ok(Self::Registry {
            name: names::normalize(input),
            index_url: None,
        })
    }

    #[must_use]
    pub fn with_index_url(self, index_url: impl Into<String>) -> Self {
        match self {
            Self::Registry { name, .. } => Self::Registry {
                name,
                index_url: Some(index_url.into()),
            },
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_registry_source_with_normalized_name() {
        assert_eq!(
            PackageSource::parse("Friendly_Bard").expect("source"),
            PackageSource::Registry {
                name: "friendly-bard".to_owned(),
                index_url: None
            }
        );
    }

    #[test]
    fn parses_url_and_path_sources() {
        assert_eq!(
            PackageSource::parse("https://example.test/pkg.whl").expect("url"),
            PackageSource::Url("https://example.test/pkg.whl".to_owned())
        );
        assert_eq!(
            PackageSource::parse("./vendor/pkg").expect("path"),
            PackageSource::Path(PathBuf::from("./vendor/pkg"))
        );
    }
}
