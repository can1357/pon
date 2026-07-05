use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::str::FromStr;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::names;
use crate::resolve::source::PackageKind;
use pep440_rs::{Version, VersionSpecifiers};
use crate::wheel::compat::{any_supported, default_supported_tags};
use crate::wheel::filename::WheelFilename;

use super::download::{HashPolicy, download_artifact};
use super::html::parse_project_html;
use super::{DistInfoMetadata, NO_OB_REFCNT_C_ABI_REFUSAL, PackageIndex, ProjectFile, ProjectPage};

const SIMPLE_JSON_ACCEPT: &str =
    "application/vnd.pypi.simple.v1+json, application/vnd.pypi.simple.v1+html;q=0.2, text/html;q=0.1";
const CACHE_SUBDIR: &str = "cache/http";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SimpleJsonIndex {
    base_url: String,
    cache_dir: PathBuf,
    artifact_cache_dir: PathBuf,
}

impl SimpleJsonIndex {
    #[must_use]
    pub fn new(base_url: impl Into<String>) -> Self {
        let cache_dir = default_cache_dir();
        let artifact_cache_dir = artifact_cache_dir_for(&cache_dir);
        Self {
            base_url: base_url.into(),
            cache_dir,
            artifact_cache_dir,
        }
    }

    #[must_use]
    pub fn with_cache_dir(base_url: impl Into<String>, cache_dir: impl Into<PathBuf>) -> Self {
        let cache_dir = cache_dir.into();
        let artifact_cache_dir = artifact_cache_dir_for(&cache_dir);
        Self {
            base_url: base_url.into(),
            cache_dir,
            artifact_cache_dir,
        }
    }

    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    #[must_use]
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    #[must_use]
    pub fn artifact_cache_dir(&self) -> &Path {
        &self.artifact_cache_dir
    }

    #[must_use]
    pub fn project_url(&self, normalized_name: &str) -> String {
        format!("{}/{normalized_name}/", self.base_url.trim_end_matches('/'))
    }

    #[must_use]
    pub fn cache_path_for_url(&self, url: &str) -> PathBuf {
        self.cache_dir.join(format!("{}.json", hex_key(url)))
    }

    pub fn lookup_cached(&self, name: impl AsRef<str>) -> Result<Option<ProjectPage>> {
        let normalized_name = validate_normalized_name(name.as_ref())?;
        let url = self.project_url(&normalized_name);
        let path = self.cache_path_for_url(&url);
        let Some(body) = read_cache_text(&path)? else {
            return Ok(None);
        };
        parse_project_response(&url, &body, None)
    }

    fn fetch_project(&self, normalized_name: &str) -> Result<Option<ProjectPage>> {
        let url = self.project_url(normalized_name);
        let cache_path = self.cache_path_for_url(&url);
        let metadata_path = metadata_path_for(&cache_path);

        if cache_is_fresh(&cache_path, &metadata_path)? {
            if let Some(body) = read_cache_text(&cache_path)? {
                return parse_project_response(&url, &body, None);
            }
            let _ = fs::remove_file(&cache_path);
        }

        let etag = cached_etag(&cache_path, &metadata_path)?;
        match fetch_simple_json(&url, etag.as_deref()) {
            Ok(FetchOutcome::Found {
                body,
                etag,
                max_age,
                content_type,
            }) => {
                write_cache_entry(&cache_path, &metadata_path, &body, etag.as_deref(), max_age)?;
                parse_project_response(&url, &body, content_type.as_deref())
            }
            Ok(FetchOutcome::NotModified) => {
                refresh_cache_metadata(&metadata_path)?;
                if let Some(body) = read_cache_text(&cache_path)? {
                    parse_project_response(&url, &body, None)
                } else {
                    let _ = fs::remove_file(&cache_path);
                    self.fetch_project(normalized_name)
                }
            }
            Ok(FetchOutcome::NotFound) => Ok(None),
            Err(error) if cache_path.is_file() => {
                let Some(body) = read_cache_text(&cache_path)? else {
                    return Err(error);
                };
                parse_project_response(&url, &body, None).map_err(|parse_error| {
                    Error::Index(format!(
                        "failed to fetch `{url}` ({error}) and cached response could not be parsed: {parse_error}"
                    ))
                })
            }
            Err(error) => Err(error),
        }
    }

    fn fetch_distribution_metadata(&self, file: &ProjectFile) -> Result<Option<String>> {
        if file.dist_info_metadata.is_none() {
            return Ok(None);
        }
        let url = metadata_url(file);
        let cache_path = self.cache_path_for_url(&url);
        let metadata_path = metadata_path_for(&cache_path);

        if cache_is_fresh(&cache_path, &metadata_path)? {
            if let Some(body) = read_cache_text(&cache_path)? {
                return Ok(Some(body));
            }
            let _ = fs::remove_file(&cache_path);
        }

        let etag = cached_etag(&cache_path, &metadata_path)?;
        match fetch_simple_json(&url, etag.as_deref()) {
            Ok(FetchOutcome::Found { body, etag, max_age, .. }) => {
                write_cache_entry(&cache_path, &metadata_path, &body, etag.as_deref(), max_age)?;
                Ok(Some(body))
            }
            Ok(FetchOutcome::NotModified) => {
                refresh_cache_metadata(&metadata_path)?;
                if let Some(body) = read_cache_text(&cache_path)? {
                    Ok(Some(body))
                } else {
                    let _ = fs::remove_file(&cache_path);
                    self.fetch_distribution_metadata(file)
                }
            }
            Ok(FetchOutcome::NotFound) => Ok(None),
            Err(error) if cache_path.is_file() => {
                let Some(body) = read_cache_text(&cache_path)? else {
                    return Err(error);
                };
                Ok(Some(body))
            }
            Err(error) => Err(error),
        }
    }
}

impl PackageIndex for SimpleJsonIndex {
    fn lookup(&self, name: &str) -> Result<Option<ProjectPage>> {
        let normalized_name = validate_normalized_name(name)?;
        self.fetch_project(&normalized_name)
    }

    fn distribution_metadata(&self, file: &ProjectFile) -> Result<Option<String>> {
        self.fetch_distribution_metadata(file)
    }

    fn fetch_artifact(&self, file: &ProjectFile) -> Result<PathBuf> {
        download_artifact(
            &self.artifact_cache_dir,
            &file.url,
            &file.filename,
            &HashPolicy::Index(&file.hashes),
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiIndex {
    indexes: Vec<SimpleJsonIndex>,
}

impl MultiIndex {
    #[must_use]
    pub fn new(indexes: Vec<SimpleJsonIndex>) -> Self {
        Self { indexes }
    }

    #[must_use]
    pub fn indexes(&self) -> &[SimpleJsonIndex] {
        &self.indexes
    }

    #[must_use]
    pub fn into_indexes(self) -> Vec<SimpleJsonIndex> {
        self.indexes
    }

    fn index_for_file(&self, file: &ProjectFile) -> Option<&SimpleJsonIndex> {
        self.indexes
            .iter()
            .find(|index| file.url.starts_with(index.base_url()))
            .or_else(|| self.indexes.first())
    }
}

impl PackageIndex for MultiIndex {
    fn lookup(&self, name: &str) -> Result<Option<ProjectPage>> {
        let normalized_name = validate_normalized_name(name)?;
        let mut merged: Option<ProjectPage> = None;

        for index in &self.indexes {
            let Some(page) = index.fetch_project(&normalized_name)? else {
                continue;
            };
            if page.name != normalized_name {
                return Err(Error::Index(format!(
                    "simple index returned project `{}` for request `{normalized_name}`",
                    page.name
                )));
            }
            match &mut merged {
                Some(existing) => existing.files.extend(page.files),
                None => merged = Some(page),
            }
        }

        Ok(merged)
    }

    fn distribution_metadata(&self, file: &ProjectFile) -> Result<Option<String>> {
        match self.index_for_file(file) {
            Some(index) => index.distribution_metadata(file),
            None => Ok(None),
        }
    }

    fn fetch_artifact(&self, file: &ProjectFile) -> Result<PathBuf> {
        let Some(index) = self.index_for_file(file) else {
            return Err(Error::Index("no simple indexes are configured".to_owned()));
        };
        index.fetch_artifact(file)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum FetchOutcome {
    Found {
        body: String,
        etag: Option<String>,
        max_age: Option<Duration>,
        content_type: Option<String>,
    },
    NotModified,
    NotFound,
}

fn fetch_simple_json(url: &str, etag: Option<&str>) -> Result<FetchOutcome> {
    let mut request = ureq::get(url).header("Accept", SIMPLE_JSON_ACCEPT);
    if let Some(etag) = etag {
        request = request.header("If-None-Match", etag);
    }
    let response = match request.call() {
        Ok(response) => response,
        Err(ureq::Error::StatusCode(304)) => return Ok(FetchOutcome::NotModified),
        Err(ureq::Error::StatusCode(404)) => return Ok(FetchOutcome::NotFound),
        Err(error) => return Err(Error::Index(format!("failed to fetch simple index `{url}`: {error}"))),
    };

    let etag = response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let max_age = response
        .headers()
        .get("cache-control")
        .and_then(|value| value.to_str().ok())
        .and_then(parse_max_age);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let mut response = response;
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|error| Error::Index(format!("failed to read simple index response `{url}`: {error}")))?;
    if body.trim().is_empty() {
        return Err(Error::Index(format!("simple index `{url}` returned an empty body")));
    }
    Ok(FetchOutcome::Found {
        body,
        etag,
        max_age,
        content_type,
    })
}

#[derive(Deserialize)]
struct SimpleProjectResponse {
    meta: SimpleMeta,
    name: String,
    #[serde(default)]
    files: Vec<SimpleFileResponse>,
}

#[derive(Deserialize)]
struct SimpleMeta {
    #[serde(rename = "api-version", alias = "_api-version")]
    api_version: String,
}

#[derive(Deserialize)]
struct SimpleFileResponse {
    filename: String,
    url: String,
    #[serde(default)]
    hashes: BTreeMap<String, String>,
    #[serde(rename = "requires-python")]
    requires_python: Option<String>,
    yanked: Option<YankedValue>,
    #[serde(rename = "dist-info-metadata")]
    dist_info_metadata: Option<MetadataValue>,
    #[serde(rename = "core-metadata")]
    core_metadata: Option<MetadataValue>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum YankedValue {
    Bool(bool),
    Reason(String),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum MetadataValue {
    Bool(bool),
    Hashes(BTreeMap<String, String>),
}

impl MetadataValue {
    fn into_metadata(self) -> Option<DistInfoMetadata> {
        match self {
            Self::Bool(false) => None,
            Self::Bool(true) => Some(DistInfoMetadata { hashes: BTreeMap::new() }),
            Self::Hashes(hashes) => Some(DistInfoMetadata { hashes }),
        }
    }
}

impl YankedValue {
    fn into_reason(self) -> Option<String> {
        match self {
            Self::Bool(false) => None,
            Self::Bool(true) => Some(String::new()),
            Self::Reason(reason) => Some(reason),
        }
    }
}

pub fn parse_project_json(body: &str) -> Result<Option<ProjectPage>> {
    let response: SimpleProjectResponse = serde_json::from_str(body)
        .map_err(|error| Error::Index(format!("invalid PEP 691 simple JSON response: {error}")))?;
    names::validate(&response.name)?;
    let name = names::normalize(&response.name);
    let files = response
        .files
        .into_iter()
        .filter_map(project_file_from_response)
        .collect::<Vec<_>>();
    Ok(Some(ProjectPage {
        meta_api_version: response.meta.api_version,
        name,
        files,
    }))
}

fn parse_project_response(url: &str, body: &str, content_type: Option<&str>) -> Result<Option<ProjectPage>> {
    let content_type = content_type.unwrap_or_default().to_ascii_lowercase();
    let trimmed = body.trim_start();
    if content_type.contains("html") || trimmed.starts_with('<') {
        return parse_project_html(url, body);
    }

    match parse_project_json(body) {
        Ok(page) => Ok(page),
        Err(_error) if !content_type.is_empty() && !content_type.contains("json") => parse_project_html(url, body),
        Err(error) => Err(error),
    }
}

fn project_file_from_response(file: SimpleFileResponse) -> Option<ProjectFile> {
    project_file_from_parts(
        file.filename,
        file.url,
        file.hashes,
        file.requires_python,
        file.yanked.and_then(YankedValue::into_reason),
        file.core_metadata
            .or(file.dist_info_metadata)
            .and_then(MetadataValue::into_metadata),
    )
}

pub(super) fn project_file_from_parts(
    filename: String,
    url: String,
    hashes: BTreeMap<String, String>,
    requires_python: Option<String>,
    yanked: Option<String>,
    dist_info_metadata: Option<DistInfoMetadata>,
) -> Option<ProjectFile> {
    let version = version_from_filename(&filename)?;
    let kind = classify_package_file(&filename);
    let (requires_python, requires_python_invalid) = match requires_python {
        Some(raw) => match VersionSpecifiers::from_str(raw.trim()) {
            Ok(specifiers) => (Some(specifiers), false),
            Err(_) => (None, true),
        },
        None => (None, false),
    };
    Some(ProjectFile {
        filename,
        url,
        version,
        kind,
        hashes,
        requires_python,
        requires_python_invalid,
        yanked,
        dist_info_metadata,
    })
}

fn classify_package_file(filename: &str) -> PackageKind {
    let Ok(wheel) = WheelFilename::parse(filename) else {
        if filename.ends_with(".tar.gz") || filename.ends_with(".zip") {
            return PackageKind::Pure;
        }
        return PackageKind::Native;
    };
    if any_supported(&wheel.tags(), &default_supported_tags()) {
        return PackageKind::Pure;
    }
    if wheel.abi_tag.split('.').any(is_refcount_cpython_abi) {
        return PackageKind::CAbiRefused {
            reason: NO_OB_REFCNT_C_ABI_REFUSAL.to_owned(),
        };
    }
    PackageKind::Native
}

fn metadata_url(file: &ProjectFile) -> String {
    format!("{}.metadata", file.url)
}

fn is_refcount_cpython_abi(tag: &str) -> bool {
    tag.starts_with("cp") && tag != "abi3" && tag != "none"
}

fn version_from_filename(filename: &str) -> Option<Version> {
    if let Ok(wheel) = WheelFilename::parse(filename) {
        return Version::from_str(&wheel.version).ok();
    }

    let stem = filename
        .strip_suffix(".tar.gz")
        .or_else(|| filename.strip_suffix(".zip"))?;
    let (_, version) = stem.rsplit_once('-')?;
    Version::from_str(version).ok()
}

fn validate_normalized_name(name: &str) -> Result<String> {
    names::validate(name)?;
    Ok(names::normalize(name))
}

fn default_cache_dir() -> PathBuf {
    let pon_home = std::env::var_os("PON_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::env::default_layout().pon_dir);
    pon_home.join(CACHE_SUBDIR)
}

fn artifact_cache_dir_for(cache_dir: &Path) -> PathBuf {
    let is_default_http_leaf = cache_dir.file_name().and_then(|name| name.to_str()) == Some("http")
        && cache_dir
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
            == Some("cache");
    if is_default_http_leaf {
        cache_dir.parent().expect("checked parent").to_path_buf()
    } else {
        cache_dir.to_path_buf()
    }
}

fn write_cache_entry(
    cache_path: &Path,
    metadata_path: &Path,
    body: &str,
    etag: Option<&str>,
    max_age: Option<Duration>,
) -> Result<()> {
    if let Some(parent) = cache_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(cache_path, body)?;
    let fetched_at = unix_now_secs()?;
    let max_age_secs = max_age.map_or(0, |duration| duration.as_secs());
    let etag = etag.unwrap_or_default();
    fs::write(metadata_path, format!("fetched_at={fetched_at}\nmax_age={max_age_secs}\netag={etag}\n"))?;
    Ok(())
}

fn read_cache_text(cache_path: &Path) -> Result<Option<String>> {
    if !cache_path.is_file() {
        return Ok(None);
    }
    let body = fs::read_to_string(cache_path)?;
    Ok((!body.trim().is_empty()).then_some(body))
}

fn cached_etag(cache_path: &Path, metadata_path: &Path) -> Result<Option<String>> {
    if !cache_path.is_file() {
        return Ok(None);
    }
    let Ok(metadata) = fs::read_to_string(metadata_path) else {
        return Ok(None);
    };
    Ok(metadata_value(&metadata, "etag")
        .filter(|etag| !etag.is_empty())
        .map(str::to_owned))
}

fn refresh_cache_metadata(metadata_path: &Path) -> Result<()> {
    let metadata = fs::read_to_string(metadata_path).unwrap_or_default();
    let max_age = metadata_value(&metadata, "max_age").unwrap_or("0");
    let etag = metadata_value(&metadata, "etag").unwrap_or_default();
    let fetched_at = unix_now_secs()?;
    fs::write(metadata_path, format!("fetched_at={fetched_at}\nmax_age={max_age}\netag={etag}\n"))?;
    Ok(())
}

fn cache_is_fresh(cache_path: &Path, metadata_path: &Path) -> Result<bool> {
    if !cache_path.is_file() {
        return Ok(false);
    }
    let Ok(metadata) = fs::read_to_string(metadata_path) else {
        return Ok(true);
    };
    let fetched_at = metadata_value(&metadata, "fetched_at").and_then(|value| value.parse::<u64>().ok());
    let max_age = metadata_value(&metadata, "max_age").and_then(|value| value.parse::<u64>().ok());
    let Some((fetched_at, max_age)) = fetched_at.zip(max_age) else {
        return Ok(true);
    };
    if max_age == 0 {
        return Ok(false);
    }
    Ok(unix_now_secs()?.saturating_sub(fetched_at) <= max_age)
}

fn metadata_value<'a>(metadata: &'a str, key: &str) -> Option<&'a str> {
    metadata.lines().find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
}

fn metadata_path_for(cache_path: &Path) -> PathBuf {
    cache_path.with_extension("meta")
}

fn parse_max_age(cache_control: &str) -> Option<Duration> {
    cache_control.split(',').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        if key.eq_ignore_ascii_case("max-age") {
            value.parse::<u64>().ok().map(Duration::from_secs)
        } else {
            None
        }
    })
}

fn unix_now_secs() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|error| Error::Index(format!("system clock is before Unix epoch: {error}")))
}

fn hex_key(url: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(url.as_bytes());
    let mut key = String::with_capacity(digest.len() * 2);
    for byte in digest {
        key.push(HEX[(byte >> 4) as usize] as char);
        key.push(HEX[(byte & 0x0f) as usize] as char);
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pep_691_fixture_into_project_files() {
        let project = parse_project_json(include_str!("fixtures/demo-pep691.json"))
            .expect("parse")
            .expect("project");

        assert_eq!(project.meta_api_version, "1.0");
        assert_eq!(project.name, "demo-pkg");
        assert_eq!(project.files.len(), 4);
        assert_eq!(project.files[0].filename, "demo_pkg-1.0.0-py3-none-any.whl");
        assert_eq!(project.files[0].version.to_string(), "1.0.0");
        assert_eq!(project.files[0].kind, PackageKind::Pure);
        assert_eq!(project.files[0].hashes["sha256"], "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(
            project.files[0].requires_python.as_ref().map(ToString::to_string),
            Some(">=3.8".to_owned())
        );
        assert!(!project.files[0].requires_python_invalid);
        assert_eq!(project.files[1].yanked.as_deref(), Some("bad metadata"));
        assert_eq!(project.files[2].kind, PackageKind::CAbiRefused {
            reason: NO_OB_REFCNT_C_ABI_REFUSAL.to_owned(),
        });
        assert_eq!(project.files[3].kind, PackageKind::Pure);
    }

    #[test]
    fn accepts_legacy_underscore_api_version_key() {
        let project = parse_project_json(
            r#"{
                "meta": {"_api-version": "1.0"},
                "name": "demo-pkg",
                "files": []
            }"#,
        )
        .expect("parse")
        .expect("project");

        assert_eq!(project.meta_api_version, "1.0");
    }

    #[test]
    fn lookup_uses_fresh_cache_without_network() {
        let temp = std::env::temp_dir().join(format!(
            "pon-simple-json-cache-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let index = SimpleJsonIndex::with_cache_dir("https://example.invalid/simple", &temp);
        let url = index.project_url("demo-pkg");
        let cache_path = index.cache_path_for_url(&url);
        fs::create_dir_all(cache_path.parent().expect("parent")).expect("mkdir");
        fs::write(&cache_path, include_str!("fixtures/demo-pep691.json")).expect("cache");
        fs::write(
            metadata_path_for(&cache_path),
            format!("fetched_at={}\nmax_age=31536000\netag=\"fixture\"\n", unix_now_secs().expect("time")),
        )
        .expect("metadata");

        let project = index.lookup("Demo_Pkg").expect("lookup").expect("project");

        assert_eq!(project.name, "demo-pkg");
        assert_eq!(
            project.files[0].requires_python.as_ref().map(ToString::to_string),
            Some(">=3.8".to_owned())
        );
    }

    #[test]
    fn cache_path_is_url_keyed_under_cache_dir() {
        let index = SimpleJsonIndex::with_cache_dir("https://pypi.example/simple/", "/tmp/pon-cache");
        let path = index.cache_path_for_url("https://pypi.example/simple/demo-pkg/");

        assert_eq!(path, PathBuf::from("/tmp/pon-cache/076d218a1c917b1bbd6081d99e9ab17dffae1a1a754da32cb695038783ec7186.json"));
    }
}
