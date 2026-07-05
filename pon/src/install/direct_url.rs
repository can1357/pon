use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DirectUrl {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_info: Option<ArchiveInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir_info: Option<DirInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vcs_info: Option<VcsInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subdirectory: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArchiveInfo {
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub hashes: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DirInfo {
    pub editable: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VcsInfo {
    pub vcs: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_revision: Option<String>,
    pub commit_id: String,
}

impl DirectUrl {
    #[must_use]
    pub fn archive(url: impl Into<String>, sha256: Option<String>) -> Self {
        let mut hashes = BTreeMap::new();
        if let Some(sha256) = sha256 {
            hashes.insert("sha256".to_owned(), sha256);
        }
        Self::archive_with_hashes(url, hashes)
    }

    #[must_use]
    pub fn archive_with_hashes(url: impl Into<String>, hashes: BTreeMap<String, String>) -> Self {
        Self {
            url: url.into(),
            archive_info: Some(ArchiveInfo { hashes }),
            dir_info: None,
            vcs_info: None,
            subdirectory: None,
        }
    }

    #[must_use]
    pub fn directory(url: impl Into<String>, editable: bool) -> Self {
        Self {
            url: url.into(),
            archive_info: None,
            dir_info: Some(DirInfo { editable }),
            vcs_info: None,
            subdirectory: None,
        }
    }

    #[must_use]
    pub fn vcs(
        url: impl Into<String>,
        vcs: impl Into<String>,
        commit_id: impl Into<String>,
        requested_revision: Option<String>,
    ) -> Self {
        Self {
            url: url.into(),
            archive_info: None,
            dir_info: None,
            vcs_info: Some(VcsInfo {
                vcs: vcs.into(),
                requested_revision,
                commit_id: commit_id.into(),
            }),
            subdirectory: None,
        }
    }

    #[must_use]
    pub fn git(url: impl Into<String>, commit_id: impl Into<String>, requested_revision: Option<String>) -> Self {
        Self::vcs(url, "git", commit_id, requested_revision)
    }

    #[must_use]
    pub fn with_subdirectory(mut self, subdirectory: impl Into<String>) -> Self {
        self.subdirectory = Some(subdirectory.into());
        self
    }

    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|error| Error::UnsupportedArtifact(format!("failed to render direct_url.json: {error}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_archive_direct_url_with_sha256_hash() {
        let direct_url = DirectUrl::archive("https://example.test/demo.whl", Some("abc123".to_owned()));

        let json = direct_url.to_json().expect("json");

        assert_eq!(
            json,
            r#"{"url":"https://example.test/demo.whl","archive_info":{"hashes":{"sha256":"abc123"}}}"#
        );
    }

    #[test]
    fn renders_editable_directory_direct_url() {
        let direct_url = DirectUrl::directory("file:///project/pkg", true);

        let json = direct_url.to_json().expect("json");

        assert_eq!(json, r#"{"url":"file:///project/pkg","dir_info":{"editable":true}}"#);
    }

    #[test]
    fn renders_git_direct_url_with_requested_revision() {
        let direct_url = DirectUrl::git(
            "git+https://example.test/repo.git",
            "0123456789abcdef",
            Some("v1.0".to_owned()),
        );

        let json = direct_url.to_json().expect("json");

        assert_eq!(
            json,
            r#"{"url":"git+https://example.test/repo.git","vcs_info":{"vcs":"git","requested_revision":"v1.0","commit_id":"0123456789abcdef"}}"#
        );
    }
}
