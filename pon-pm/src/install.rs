use std::fs;
use std::path::{Path, PathBuf};

use crate::env::EnvLayout;
use crate::error::{Error, Result};
use crate::names;
use crate::wheel::record::parse_record;

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
    pub record_path: Option<PathBuf>,
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

pub fn remove_installed_package(env: &EnvLayout, name: &str) -> Result<Option<InstalledPackageRecord>> {
    let mut records = read_installed_packages(env)?;
    let normalized = names::normalize(name);
    let Some(index) = records
        .iter()
        .position(|candidate| names::normalize(&candidate.name) == normalized)
    else {
        return Ok(None);
    };
    let record = records.remove(index);
    if let Some(record_path) = &record.record_path {
        remove_record_files(env, record_path)?;
    }
    write_installed_packages(env, &records)?;
    Ok(Some(record))
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

fn remove_record_files(env: &EnvLayout, record_path: &Path) -> Result<()> {
    let record_relative = registry_relative_path(record_path, "RECORD path")?;
    let record_full_path = env.site_packages.join(record_relative);
    let record_text = fs::read_to_string(&record_full_path)?;
    let entries = parse_record(&record_text, &record_full_path.display().to_string())?;
    for entry in entries {
        let relative_path = registry_relative_path(Path::new(&entry.path), "RECORD entry")?;
        let full_path = env.site_packages.join(relative_path);
        if full_path.is_file() || full_path.is_symlink() {
            fs::remove_file(&full_path)?;
            prune_empty_parents(&env.site_packages, full_path.parent());
        } else if full_path.is_dir() {
            match fs::remove_dir(&full_path) {
                Ok(()) => prune_empty_parents(&env.site_packages, full_path.parent()),
                Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}

fn prune_empty_parents(root: &Path, start: Option<&Path>) {
    let Some(mut current) = start else {
        return;
    };
    while current != root && current.starts_with(root) {
        match fs::remove_dir(current) {
            Ok(()) => {
                let Some(parent) = current.parent() else {
                    return;
                };
                current = parent;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::DirectoryNotEmpty | std::io::ErrorKind::NotFound
                ) =>
            {
                return;
            }
            Err(_) => return,
        }
    }
}

fn registry_relative_path(path: &Path, label: &str) -> Result<PathBuf> {
    if path.is_absolute() {
        return Err(Error::UnsupportedArtifact(format!(
            "{label} `{}` cannot be absolute",
            path.display()
        )));
    }
    let text = path.to_string_lossy();
    if text.is_empty() || text.contains('\\') {
        return Err(Error::UnsupportedArtifact(format!("unsafe {label} `{}`", path.display())));
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => out.push(part),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir | std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err(Error::UnsupportedArtifact(format!("unsafe {label} `{}`", path.display())));
            }
        }
    }
    if out.as_os_str().is_empty() {
        return Err(Error::UnsupportedArtifact(format!("unsafe {label} `{}`", path.display())));
    }
    Ok(out)
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
        if !matches!(fields.len(), 4 | 5) {
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
            record_path: fields.get(4).and_then(|field| (!field.is_empty()).then(|| PathBuf::from(field))),
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
        let record_path = record
            .record_path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        validate_registry_field(&record_path, "RECORD path")?;
        if !record_path.is_empty() {
            registry_relative_path(Path::new(&record_path), "RECORD path")?;
        }
        content.push_str(&record.name);
        content.push('\t');
        content.push_str(&record.version);
        content.push('\t');
        content.push_str(&record.artifact_kind);
        content.push('\t');
        content.push_str(&record.import_names.join(","));
        content.push('\t');
        content.push_str(&record_path);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wheel::install_wheel;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn registry_reader_accepts_legacy_four_column_rows() {
        let layout = EnvLayout::new(temp_project("legacy"));
        fs::create_dir_all(layout.registry_path.parent().expect("parent")).expect("parent");
        fs::write(&layout.registry_path, "idna\t3.9\twheel\tidna\n").expect("registry");

        let records = read_installed_packages(&layout).expect("records");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].record_path, None);
    }

    #[test]
    fn registry_round_trips_record_path_column() {
        let layout = EnvLayout::new(temp_project("registry-v2"));
        write_installed_packages(
            &layout,
            &[InstalledPackageRecord {
                name: "idna".to_owned(),
                version: "3.10".to_owned(),
                artifact_kind: "wheel".to_owned(),
                import_names: vec!["idna".to_owned()],
                record_path: Some(PathBuf::from("idna-3.10.dist-info/RECORD")),
            }],
        )
        .expect("write");

        let raw = fs::read_to_string(&layout.registry_path).expect("raw");
        assert_eq!(raw, "idna\t3.10\twheel\tidna\tidna-3.10.dist-info/RECORD\n");
        let records = read_installed_packages(&layout).expect("read");
        assert_eq!(records[0].record_path.as_deref(), Some(Path::new("idna-3.10.dist-info/RECORD")));
    }

    #[test]
    fn remove_uses_record_file_set_and_prunes_empty_parents() {
        let layout = EnvLayout::new(temp_project("remove"));
        let record = ResolvedRecord::wheel("idna", "3.10", "idna-3.10-py3-none-any.whl");
        install_wheel(&layout, &record, "idna-3.10-py3-none-any.whl").expect("install");

        let removed = remove_installed_package(&layout, "idna").expect("remove");

        assert_eq!(removed.expect("removed").name, "idna");
        assert!(read_installed_packages(&layout).expect("registry").is_empty());
        assert!(fs::read_dir(&layout.site_packages).expect("site-packages").next().is_none());
    }

    fn temp_project(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("pon-pm-install-{label}-{unique}"))
    }
}
