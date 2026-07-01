use std::fs;
use std::path::{Path, PathBuf};

use crate::env::EnvLayout;
use crate::error::{Error, Result};
use crate::names;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedRecord {
    pub name: String,
    pub version: String,
    pub artifact: PackageArtifact,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackageArtifact {
    Wheel { filename: String },
    Sdist { filename: String },
    LocalPath { path: PathBuf },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallReport {
    pub package_name: String,
    pub version: String,
    pub artifact_kind: String,
    pub import_names: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledPackageRecord {
    pub name: String,
    pub version: String,
    pub artifact_kind: String,
    pub import_names: Vec<String>,
}

impl ResolvedRecord {
    #[must_use]
    pub fn wheel(name: impl Into<String>, version: impl Into<String>, filename: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            artifact: PackageArtifact::Wheel {
                filename: filename.into(),
            },
        }
    }

    #[must_use]
    pub fn sdist(name: impl Into<String>, version: impl Into<String>, filename: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            artifact: PackageArtifact::Sdist {
                filename: filename.into(),
            },
        }
    }

    #[must_use]
    pub fn local_path(name: impl Into<String>, version: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            artifact: PackageArtifact::LocalPath { path: path.into() },
        }
    }

    #[must_use]
    pub fn normalized_name(&self) -> String {
        names::normalize(&self.name)
    }
}

pub fn install_package(env: &EnvLayout, resolved_record: &ResolvedRecord) -> Result<InstallReport> {
    match &resolved_record.artifact {
        PackageArtifact::Wheel { filename } => crate::wheel::install_wheel(env, resolved_record, filename),
        PackageArtifact::Sdist { filename } => crate::sdist::install_sdist(env, resolved_record, filename),
        PackageArtifact::LocalPath { path } => crate::native::install_local_package(env, resolved_record, path),
    }
}

pub fn read_installed_packages(env: &EnvLayout) -> Result<Vec<InstalledPackageRecord>> {
    read_installed_packages_from_path(&env.registry_path)
}

pub fn write_installed_packages(env: &EnvLayout, records: &[InstalledPackageRecord]) -> Result<()> {
    write_installed_packages_to_path(&env.registry_path, records)
}

pub(crate) fn upsert_installed_package(env: &EnvLayout, record: InstalledPackageRecord) -> Result<()> {
    let mut records = read_installed_packages(env)?;
    let normalized = names::normalize(&record.name);
    if let Some(existing) = records
        .iter_mut()
        .find(|candidate| names::normalize(&candidate.name) == normalized)
    {
        *existing = record;
    } else {
        records.push(record);
    }
    records.sort_by(|left, right| names::normalize(&left.name).cmp(&names::normalize(&right.name)));
    write_installed_packages(env, &records)
}

fn read_installed_packages_from_path(path: &Path) -> Result<Vec<InstalledPackageRecord>> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut records = Vec::new();
    for (line_index, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 4 {
            return Err(Error::UnsupportedArtifact(format!(
                "invalid package registry line {} in {}",
                line_index + 1,
                path.display()
            )));
        }
        let import_names = if fields[3].is_empty() {
            Vec::new()
        } else {
            fields[3].split(',').map(str::to_owned).collect()
        };
        records.push(InstalledPackageRecord {
            name: fields[0].to_owned(),
            version: fields[1].to_owned(),
            artifact_kind: fields[2].to_owned(),
            import_names,
        });
    }
    Ok(records)
}

fn write_installed_packages_to_path(path: &Path, records: &[InstalledPackageRecord]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut content = String::new();
    for record in records {
        validate_registry_field(&record.name, "package name")?;
        validate_registry_field(&record.version, "package version")?;
        validate_registry_field(&record.artifact_kind, "artifact kind")?;
        for import_name in &record.import_names {
            validate_registry_field(import_name, "import name")?;
            if import_name.contains(',') {
                return Err(Error::UnsupportedArtifact(format!(
                    "import name `{import_name}` cannot be written to package registry"
                )));
            }
        }
        content.push_str(&record.name);
        content.push('\t');
        content.push_str(&record.version);
        content.push('\t');
        content.push_str(&record.artifact_kind);
        content.push('\t');
        content.push_str(&record.import_names.join(","));
        content.push('\n');
    }
    fs::write(path, content)?;
    Ok(())
}

pub(crate) fn validate_registry_field(value: &str, label: &str) -> Result<()> {
    if value.contains(['\t', '\n', '\r']) {
        return Err(Error::UnsupportedArtifact(format!(
            "{label} `{value}` cannot contain registry delimiters"
        )));
    }
    Ok(())
}
