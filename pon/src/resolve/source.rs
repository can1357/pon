use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use flate2::read::GzDecoder;
use pep440_rs::{Version, VersionSpecifiers};
use zip::ZipArchive;
use zip::result::ZipError;

use crate::error::{Error, Result};
use crate::index::{PackageIndex, ProjectFile, ProjectPage};
use crate::marker::pon_marker_env;
use crate::metadata::{CoreMetadata, parse_core_metadata};
use crate::names;
use crate::pyproject::PyProject;

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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ArtifactSet {
    pub wheels: Vec<ProjectFile>,
    pub sdist: Option<ProjectFile>,
}

pub trait CandidateSource {
    fn available_versions(&self, name: &str, include_yanked: bool) -> Result<Vec<Version>>;
    fn artifacts(&self, name: &str, version: &Version) -> Result<ArtifactSet>;
    fn metadata(&self, name: &str, version: &Version) -> Result<CoreMetadata>;
}

pub struct IndexSource<'a, I: PackageIndex> {
    index: &'a I,
    pages: RefCell<HashMap<String, Option<ProjectPage>>>,
}

impl<'a, I: PackageIndex> IndexSource<'a, I> {
    #[must_use]
    pub fn new(index: &'a I) -> Self {
        Self {
            index,
            pages: RefCell::new(HashMap::new()),
        }
    }

    fn project_page(&self, name: &str) -> Result<Option<ProjectPage>> {
        let normalized = names::normalize(name);
        if let Some(project) = self.pages.borrow().get(&normalized) {
            return Ok(project.clone());
        }

        let project = self.index.lookup(&normalized)?;
        self.pages.borrow_mut().insert(normalized, project.clone());
        Ok(project)
    }
}

impl<I: PackageIndex> CandidateSource for IndexSource<'_, I> {
    fn available_versions(&self, name: &str, include_yanked: bool) -> Result<Vec<Version>> {
        let Some(project) = self.project_page(name)? else {
            return Ok(Vec::new());
        };

        let mut versions = project
            .files
            .iter()
            .filter(|file| include_yanked || file.yanked.is_none())
            .map(|file| file.version.clone())
            .collect::<Vec<_>>();
        versions.sort();
        versions.dedup();
        Ok(versions)
    }

    fn artifacts(&self, name: &str, version: &Version) -> Result<ArtifactSet> {
        let Some(project) = self.project_page(name)? else {
            return Ok(ArtifactSet::default());
        };

        let marker_env = pon_marker_env();
        let python_version = marker_env.python_version().version.clone();
        let mut artifacts = ArtifactSet::default();

        for file in project.files.into_iter().filter(|file| &file.version == version) {
            if file.requires_python_invalid
                || !requires_python_matches(file.requires_python.as_ref(), &python_version)
            {
                continue;
            }

            if is_sdist_filename(&file.filename) {
                if artifacts.sdist.is_none() {
                    artifacts.sdist = Some(file);
                }
            } else if is_wheel_filename(&file.filename) {
                artifacts.wheels.push(file);
            }
        }
        artifacts.wheels.sort_by(|left, right| {
            wheel_installability_rank(left)
                .cmp(&wheel_installability_rank(right))
                .then_with(|| left.filename.cmp(&right.filename))
        });
        Ok(artifacts)
    }

    fn metadata(&self, name: &str, version: &Version) -> Result<CoreMetadata> {
        let artifacts = self.artifacts(name, version)?;

        for file in artifacts.wheels.iter().chain(artifacts.sdist.iter()) {
            if let Some(metadata) = self.index.distribution_metadata(file)? {
                return parse_core_metadata(&metadata, &file.filename);
            }
        }

        if let Some(file) = artifacts.wheels.first() {
            let path = self.index.fetch_artifact(file)?;
            let metadata = wheel_metadata_text(file, &path)?;
            return parse_core_metadata(&metadata, &file.filename);
        }

        if let Some(file) = artifacts.sdist.as_ref() {
            let path = self.index.fetch_artifact(file)?;
            let metadata = sdist_metadata_text(file, &path)?;
            return parse_core_metadata(&metadata, &file.filename);
        }

        Err(Error::UnsupportedArtifact(format!(
            "no metadata artifact is available for `{name}` {version}"
        )))
    }
}

fn requires_python_matches(requires_python: Option<&VersionSpecifiers>, python_version: &Version) -> bool {
    match requires_python {
        Some(specifiers) => specifiers.contains(python_version),
        None => true,
    }
}

fn is_wheel_filename(filename: &str) -> bool {
    filename.ends_with(".whl")
}

fn is_sdist_filename(filename: &str) -> bool {
    filename.ends_with(".tar.gz") || filename.ends_with(".zip")
}

fn wheel_installability_rank(file: &ProjectFile) -> u8 {
    match &file.kind {
        PackageKind::Pure => 0,
        PackageKind::Native => 1,
        PackageKind::CAbiRefused { .. } => 2,
    }
}

fn wheel_metadata_text(file: &ProjectFile, path: &Path) -> Result<String> {
    let label = path.display().to_string();
    let archive_file = File::open(path)?;
    let mut archive = ZipArchive::new(archive_file).map_err(|error| zip_error(&label, error))?;
    for index in 0..archive.len() {
        let mut member = archive.by_index(index).map_err(|error| zip_error(&label, error))?;
        if member.name().ends_with(".dist-info/METADATA") {
            let mut metadata = String::new();
            member.read_to_string(&mut metadata)?;
            return Ok(metadata);
        }
    }

    Err(Error::UnsupportedArtifact(format!(
        "wheel `{}` does not contain a .dist-info/METADATA member",
        file.filename
    )))
}

fn sdist_metadata_text(file: &ProjectFile, path: &Path) -> Result<String> {
    if file.filename.ends_with(".tar.gz") {
        read_tar_gz_sdist_metadata(file, path)
    } else if file.filename.ends_with(".zip") {
        read_zip_sdist_metadata(file, path)
    } else {
        Err(Error::UnsupportedArtifact(format!(
            "artifact `{}` is not a supported sdist archive",
            file.filename
        )))
    }
}

fn read_tar_gz_sdist_metadata(file: &ProjectFile, path: &Path) -> Result<String> {
    let archive_file = File::open(path)?;
    let decoder = GzDecoder::new(archive_file);
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|error| sdist_read_error(file, error))?;
    let mut pyproject = None;

    for entry in entries {
        let mut entry = entry.map_err(|error| sdist_read_error(file, error))?;
        let entry_path = entry
            .path()
            .map_err(|error| sdist_read_error(file, error))?
            .into_owned();
        let filename = entry_path.file_name().and_then(|name| name.to_str());

        if filename == Some("PKG-INFO") {
            let mut metadata = String::new();
            entry
                .read_to_string(&mut metadata)
                .map_err(|error| sdist_read_error(file, error))?;
            return Ok(metadata);
        }

        if filename == Some("pyproject.toml") && pyproject.is_none() {
            let mut content = String::new();
            entry
                .read_to_string(&mut content)
                .map_err(|error| sdist_read_error(file, error))?;
            pyproject = Some((format!("{}:{}", file.filename, entry_path.display()), content));
        }
    }

    pyproject
        .map(|(label, content)| pyproject_core_metadata_text(file, &label, &content))
        .unwrap_or_else(|| missing_sdist_metadata(file))
}

fn read_zip_sdist_metadata(file: &ProjectFile, path: &Path) -> Result<String> {
    let archive_file = File::open(path)?;
    let mut archive = ZipArchive::new(archive_file).map_err(|error| zip_sdist_error(file, error))?;
    let mut pyproject = None;

    for index in 0..archive.len() {
        let mut member = archive.by_index(index).map_err(|error| zip_sdist_error(file, error))?;
        let member_name = member.name().to_owned();
        let filename = Path::new(&member_name).file_name().and_then(|name| name.to_str());

        if filename == Some("PKG-INFO") {
            let mut metadata = String::new();
            member.read_to_string(&mut metadata).map_err(|error| sdist_read_error(file, error))?;
            return Ok(metadata);
        }

        if filename == Some("pyproject.toml") && pyproject.is_none() {
            let mut content = String::new();
            member.read_to_string(&mut content).map_err(|error| sdist_read_error(file, error))?;
            pyproject = Some((format!("{}:{member_name}", file.filename), content));
        }
    }

    pyproject
        .map(|(label, content)| pyproject_core_metadata_text(file, &label, &content))
        .unwrap_or_else(|| missing_sdist_metadata(file))
}

fn pyproject_core_metadata_text(file: &ProjectFile, label: &str, content: &str) -> Result<String> {
    let pyproject = PyProject::from_str(label, content)?;
    let name = pyproject.project_name().ok_or_else(|| {
        Error::UnsupportedArtifact(format!(
            "sdist `{}` pyproject.toml is missing [project].name",
            file.filename
        ))
    })?;
    let version = pyproject.project_version().ok_or_else(|| {
        Error::UnsupportedArtifact(format!(
            "sdist `{}` pyproject.toml is missing [project].version",
            file.filename
        ))
    })?;

    let mut metadata = format!("Metadata-Version: 2.1\nName: {name}\nVersion: {version}\n");
    for dependency in pyproject.dependencies() {
        metadata.push_str("Requires-Dist: ");
        metadata.push_str(&dependency);
        metadata.push('\n');
    }
    Ok(metadata)
}

fn missing_sdist_metadata(file: &ProjectFile) -> Result<String> {
    Err(Error::UnsupportedArtifact(format!(
        "sdist `{}` contains neither PKG-INFO nor static pyproject.toml metadata",
        file.filename
    )))
}

fn sdist_read_error(file: &ProjectFile, error: impl std::fmt::Display) -> Error {
    Error::UnsupportedArtifact(format!("failed to read sdist `{}`: {error}", file.filename))
}

fn zip_sdist_error(file: &ProjectFile, error: ZipError) -> Error {
    Error::UnsupportedArtifact(format!("failed to read sdist `{}`: {error}", file.filename))
}

fn zip_error(label: &str, error: ZipError) -> Error {
    Error::UnsupportedArtifact(format!("failed to read wheel `{label}`: {error}"))
}

