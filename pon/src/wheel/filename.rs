use std::path::Path;

use crate::error::{Error, Result};
use crate::names;

use super::compat::{Tag, parse_tag_set};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WheelFilename {
    pub distribution: String,
    pub normalized_distribution: String,
    pub version: String,
    pub build: Option<String>,
    pub python_tag: String,
    pub abi_tag: String,
    pub platform_tag: String,
}

impl WheelFilename {
    pub fn parse(filename: impl AsRef<str>) -> Result<Self> {
        let filename = filename.as_ref();
        let basename = Path::new(filename)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(filename);
        let stem = basename
            .strip_suffix(".whl")
            .ok_or_else(|| Error::InvalidWheelFilename(filename.to_owned()))?;
        let parts = stem.split('-').collect::<Vec<_>>();
        let (distribution, version, build, python_tag, abi_tag, platform_tag) = match parts.as_slice() {
            [distribution, version, python_tag, abi_tag, platform_tag] => {
                (*distribution, *version, None, *python_tag, *abi_tag, *platform_tag)
            }
            [distribution, version, build, python_tag, abi_tag, platform_tag] => {
                (*distribution, *version, Some((*build).to_owned()), *python_tag, *abi_tag, *platform_tag)
            }
            _ => return Err(Error::InvalidWheelFilename(filename.to_owned())),
        };

        if distribution.is_empty()
            || version.is_empty()
            || python_tag.is_empty()
            || abi_tag.is_empty()
            || platform_tag.is_empty()
        {
            return Err(Error::InvalidWheelFilename(filename.to_owned()));
        }

        Ok(Self {
            distribution: distribution.to_owned(),
            normalized_distribution: names::normalize(distribution),
            version: version.to_owned(),
            build,
            python_tag: python_tag.to_owned(),
            abi_tag: abi_tag.to_owned(),
            platform_tag: platform_tag.to_owned(),
        })
    }

    #[must_use]
    pub fn tags(&self) -> Vec<Tag> {
        parse_tag_set(&self.python_tag, &self.abi_tag, &self.platform_tag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wheel::compat::{any_supported, default_supported_tags};

    #[test]
    fn parses_pure_python_wheel_filename() {
        let wheel = WheelFilename::parse("Friendly_Bard-1.2.3-py3-none-any.whl").expect("wheel");
        assert_eq!(wheel.normalized_distribution, "friendly-bard");
        assert_eq!(wheel.version, "1.2.3");
        assert_eq!(wheel.build, None);
        assert!(any_supported(&wheel.tags(), &default_supported_tags()));
    }

    #[test]
    fn parses_build_tag() {
        let wheel = WheelFilename::parse("demo-1.0-2-py3-none-any.whl").expect("wheel");
        assert_eq!(wheel.build.as_deref(), Some("2"));
    }

    #[test]
    fn rejects_non_wheel_filename() {
        assert!(WheelFilename::parse("demo.tar.gz").is_err());
    }
}
