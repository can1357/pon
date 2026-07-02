//! Core metadata parsing for Python distribution metadata.

use std::str::FromStr;

use pep440_rs::{Version, VersionSpecifiers};
use pep508_rs::{ExtraName, Requirement};

use crate::error::{Error, Result};

/// Parsed RFC-822 core metadata needed by the resolver and package commands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoreMetadata {
    pub metadata_version: String,
    pub name: String,
    pub version: Version,
    pub requires_dist: Vec<Requirement>,
    pub requires_python: Option<VersionSpecifiers>,
    pub provides_extra: Vec<ExtraName>,
    pub summary: Option<String>,
    pub license: Option<String>,
    pub author: Option<String>,
    pub author_email: Option<String>,
    pub home_page: Option<String>,
    pub project_urls: Vec<String>,
    pub classifiers: Vec<String>,
    pub dynamic: Vec<String>,
}

/// Parse the header block of a Python core metadata document.
pub fn parse_core_metadata(text: &str, label: &str) -> Result<CoreMetadata> {
    let headers = parse_headers(text);
    let name = required_header(&headers, "Name", label)?;
    let version_raw = required_header(&headers, "Version", label)?;
    let version = Version::from_str(&version_raw).map_err(|_| {
        Error::UnsupportedArtifact(format!(
            "{label} has invalid core metadata field `Version`: {version_raw}"
        ))
    })?;

    let requires_dist = headers
        .values("Requires-Dist")
        .into_iter()
        .map(|value| {
            Requirement::from_str(&value).map_err(|_| {
                Error::UnsupportedArtifact(format!(
                    "{label} has invalid Requires-Dist value `{value}`"
                ))
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let requires_python = match headers.value("Requires-Python") {
        Some(value) => Some(VersionSpecifiers::from_str(&value).map_err(|_| {
            Error::UnsupportedArtifact(format!(
                "{label} has invalid Requires-Python value `{value}`"
            ))
        })?),
        None => None,
    };

    let provides_extra = headers
        .values("Provides-Extra")
        .into_iter()
        .map(|value| {
            ExtraName::from_str(&value).map_err(|_| {
                Error::UnsupportedArtifact(format!(
                    "{label} has invalid Provides-Extra value `{value}`"
                ))
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(CoreMetadata {
        metadata_version: headers.value("Metadata-Version").unwrap_or_default(),
        name,
        version,
        requires_dist,
        requires_python,
        provides_extra,
        summary: headers.value("Summary"),
        license: headers.value("License"),
        author: headers.value("Author"),
        author_email: headers.value("Author-email"),
        home_page: headers.value("Home-page"),
        project_urls: headers.values("Project-URL"),
        classifiers: headers.values("Classifier"),
        dynamic: headers.values("Dynamic"),
    })
}

fn required_header(headers: &Headers, key: &str, label: &str) -> Result<String> {
    headers.value(key).ok_or_else(|| {
        Error::UnsupportedArtifact(format!(
            "{label} is missing core metadata field `{key}`"
        ))
    })
}

#[derive(Default)]
struct Headers {
    fields: Vec<(String, String)>,
}

impl Headers {
    fn value(&self, key: &str) -> Option<String> {
        self.values(key).into_iter().next()
    }

    fn values(&self, key: &str) -> Vec<String> {
        self.fields
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, value)| value.clone())
            .collect()
    }
}

fn parse_headers(text: &str) -> Headers {
    let mut headers = Headers::default();
    for line in text.lines() {
        if line.is_empty() {
            break;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some((_, value)) = headers.fields.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        headers
            .fields
            .push((name.trim().to_owned(), value.trim().to_owned()));
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_core_metadata_headers_and_continuations() {
        let metadata = "Metadata-Version: 2.1\nName: Demo\nVersion: 1.0\nRequires-Dist: idna >=3\nRequires-Dist: docs ;\n extra == 'docs'\nRequires-Python: >=3.12\nProvides-Extra: docs\nClassifier: Programming Language :: Python\n\nbody ignored\n";

        let parsed = parse_core_metadata(metadata, "demo.whl").expect("metadata");

        assert_eq!(parsed.name, "Demo");
        assert_eq!(parsed.version.to_string(), "1.0");
        assert_eq!(parsed.requires_dist.len(), 2);
        assert_eq!(parsed.requires_python.expect("requires-python").to_string(), ">=3.12");
        assert_eq!(parsed.provides_extra[0].as_ref(), "docs");
        assert_eq!(parsed.classifiers, vec!["Programming Language :: Python"]);
    }

    #[test]
    fn requires_name_and_version() {
        let error = parse_core_metadata("Metadata-Version: 2.1\nVersion: 1\n", "missing").expect_err("error");
        assert!(error.to_string().contains("missing core metadata field `Name`"));
    }
}
