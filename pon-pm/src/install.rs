pub mod direct_url;

use std::fs;
use std::path::{Component, Path, PathBuf};
use self::direct_url::DirectUrl;

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
    Wheel { path: PathBuf, direct_url: Option<DirectUrl> },
    Sdist { path: PathBuf, direct_url: Option<DirectUrl> },
    Dir { path: PathBuf, editable: bool },
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
    pub fn wheel(name: impl Into<String>, version: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            artifact: PackageArtifact::Wheel {
                path: path.into(),
                direct_url: None,
            },
        }
    }

    #[must_use]
    pub fn sdist(name: impl Into<String>, version: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            artifact: PackageArtifact::Sdist {
                path: path.into(),
                direct_url: None,
            },
        }
    }

    #[must_use]
    pub fn dir(name: impl Into<String>, version: impl Into<String>, path: impl Into<PathBuf>, editable: bool) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            artifact: PackageArtifact::Dir {
                path: path.into(),
                editable,
            },
        }
    }

    #[must_use]
    pub fn local_path(name: impl Into<String>, version: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self::dir(name, version, path, false)
    }

    #[must_use]
    pub fn normalized_name(&self) -> String {
        names::normalize(&self.name)
    }
}

pub fn install_package(env: &EnvLayout, resolved_record: &ResolvedRecord) -> Result<InstallReport> {
    let normalized = resolved_record.normalized_name();
    let existing = read_installed_packages(env)?
        .into_iter()
        .find(|candidate| names::normalize(&candidate.name) == normalized);

    if let Some(existing) = existing {
        if existing.version == resolved_record.version {
            return Ok(InstallReport {
                package_name: existing.name,
                version: existing.version,
                artifact_kind: existing.artifact_kind,
                import_names: existing.import_names,
            });
        }
        remove_installed_package(env, &existing.name)?;
    }

    match &resolved_record.artifact {
        PackageArtifact::Wheel { path, direct_url } => {
            crate::wheel::install_wheel(env, resolved_record, path, direct_url.as_ref())
        }
        PackageArtifact::Sdist { path, direct_url: _ } => {
            let filename = path.to_string_lossy();
            crate::sdist::install_sdist(env, resolved_record, &filename)
        }
        PackageArtifact::Dir { path, editable } => {
            if *editable {
                crate::editable::install_editable(env, resolved_record, path)
            } else {
                install_non_editable_dir(env, resolved_record, path)
            }
        }
    }
}

fn install_non_editable_dir(env: &EnvLayout, resolved_record: &ResolvedRecord, path: &Path) -> Result<InstallReport> {
    let pyproject = crate::pyproject::PyProject::read(path.join("pyproject.toml"))?;
    if pyproject.tool_pon_native_import_name().is_some() {
        crate::native::install_local_package(env, resolved_record, path)
    } else {
        crate::local::install_local_python_package(env, resolved_record, path)
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
    remove_installed_files(env, &record)?;
    write_installed_packages(env, &records)?;
    Ok(Some(record))
}

fn remove_installed_files(env: &EnvLayout, record: &InstalledPackageRecord) -> Result<()> {
    if let Some(record_path) = &record.record_path {
        return remove_record_files(env, record_path);
    }
    if let Some(record_path) = find_legacy_record_path(env, record)? {
        return remove_record_files(env, &record_path);
    }
    remove_import_roots(env, record)?;
    eprintln!("warning: no RECORD for {}; removed import roots only", record.name);
    Ok(())
}

fn find_legacy_record_path(env: &EnvLayout, record: &InstalledPackageRecord) -> Result<Option<PathBuf>> {
    let entries = match fs::read_dir(&env.site_packages) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let normalized_name = names::normalize(&record.name);
    let mut candidates = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let Some(stem) = file_name.strip_suffix(".dist-info") else {
            continue;
        };
        let Some((dist_name, version)) = stem.rsplit_once('-') else {
            continue;
        };
        if names::normalize(dist_name) != normalized_name || version != record.version {
            continue;
        }
        let relative = PathBuf::from(file_name).join("RECORD");
        if env.site_packages.join(&relative).is_file() {
            candidates.push(relative);
        }
    }
    candidates.sort();
    Ok(candidates.into_iter().next())
}

fn remove_import_roots(env: &EnvLayout, record: &InstalledPackageRecord) -> Result<()> {
    for import_name in &record.import_names {
        let root = import_root(import_name)?;
        remove_import_root_path(env, &env.site_packages.join(root))?;
        remove_import_root_path(env, &env.site_packages.join(format!("{root}.py")))?;
    }
    Ok(())
}

fn import_root(import_name: &str) -> Result<&str> {
    let root = import_name.split('.').next().unwrap_or(import_name);
    if root.is_empty() || root.contains(['/', '\\']) || root == "." || root == ".." {
        return Err(Error::UnsupportedArtifact(format!(
            "unsafe import root `{import_name}` in package registry"
        )));
    }
    Ok(root)
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
    let record_full_path = record_member_path(env, record_path, "RECORD path")?;
    let record_text = fs::read_to_string(&record_full_path)?;
    let entries = parse_record(&record_text, &record_full_path.display().to_string())?;
    for entry in entries {
        let full_path = record_member_path(env, Path::new(&entry.path), "RECORD entry")?;
        remove_record_entry_path(env, &full_path)?;
    }
    Ok(())
}

fn remove_record_entry_path(env: &EnvLayout, path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let file_type = metadata.file_type();
    if file_type.is_symlink() || file_type.is_file() {
        fs::remove_file(path)?;
        prune_after_removal(env, path);
    } else if file_type.is_dir() {
        match fs::remove_dir(path) {
            Ok(()) => prune_after_removal(env, path),
            Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn remove_import_root_path(env: &EnvLayout, path: &Path) -> Result<()> {
    let path = normalized_path(path);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let file_type = metadata.file_type();
    if file_type.is_dir() && !file_type.is_symlink() {
        fs::remove_dir_all(&path)?;
    } else {
        fs::remove_file(&path)?;
    }
    prune_after_removal(env, &path);
    Ok(())
}

fn prune_after_removal(env: &EnvLayout, path: &Path) {
    let root = prune_root_for_path(env, path);
    prune_empty_parents(&root, path.parent());
}

fn prune_root_for_path(env: &EnvLayout, path: &Path) -> PathBuf {
    let site_packages = normalized_path(&env.site_packages);
    if path.starts_with(&site_packages) {
        return site_packages;
    }
    let scripts_dir = normalized_path(&env.scripts_dir);
    if path.starts_with(&scripts_dir) {
        return scripts_dir;
    }
    normalized_path(&env.pon_dir)
}

fn record_member_path(env: &EnvLayout, path: &Path, label: &str) -> Result<PathBuf> {
    if path.is_absolute() {
        return Err(Error::UnsupportedArtifact(format!(
            "{label} `{}` cannot be absolute",
            path.display()
        )));
    }
    let text = path.to_string_lossy();
    if text.is_empty() || text.contains('\\') {
        return Err(unsafe_record_path(label, path));
    }

    let pon_root = normalized_path(&env.pon_dir);
    let site_root = normalized_path(&env.site_packages);
    let packages_root = normalized_path(&env.packages_dir);
    let scripts_root = normalized_path(&env.scripts_dir);
    let mut full_path = site_root.clone();
    for component in path.components() {
        match component {
            Component::Normal(part) => full_path.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                if !full_path.pop() || !full_path.starts_with(&pon_root) {
                    return Err(unsafe_record_path(label, path));
                }
            }
            Component::RootDir | Component::Prefix(_) => return Err(unsafe_record_path(label, path)),
        }
    }
    if !full_path.starts_with(&pon_root)
        || full_path == pon_root
        || full_path == packages_root
        || full_path == site_root
        || full_path == scripts_root
    {
        return Err(unsafe_record_path(label, path));
    }
    Ok(full_path)
}

fn unsafe_record_path(label: &str, path: &Path) -> Error {
    Error::UnsupportedArtifact(format!("unsafe {label} `{}`", path.display()))
}

fn normalized_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
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
        let wheel_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join("wheels")
            .join("idna-3.10-py3-none-any.whl");
        install_wheel(&layout, &record, &wheel_path, None).expect("install");

        let removed = remove_installed_package(&layout, "idna").expect("remove");

        assert_eq!(removed.expect("removed").name, "idna");
        assert!(read_installed_packages(&layout).expect("registry").is_empty());
        assert!(fs::read_dir(&layout.site_packages).expect("site-packages").next().is_none());
    }

    #[test]
    fn remove_record_files_allows_parent_entries_under_pon() {
        let layout = EnvLayout::new(temp_project("record-parent"));
        fs::create_dir_all(layout.site_packages.join("demo-1.0.dist-info")).expect("dist-info");
        fs::create_dir_all(&layout.scripts_dir).expect("scripts");
        fs::write(layout.scripts_dir.join("demo"), "script").expect("script");
        fs::write(
            layout.site_packages.join("demo-1.0.dist-info/RECORD"),
            "../../bin/demo,,\ndemo-1.0.dist-info/RECORD,,\n",
        )
        .expect("record");

        remove_record_files(&layout, Path::new("demo-1.0.dist-info/RECORD")).expect("remove");

        assert!(!layout.scripts_dir.join("demo").exists());
        assert!(layout.scripts_dir.exists());
        assert!(!layout.site_packages.join("demo-1.0.dist-info").exists());
    }

    #[test]
    fn remove_record_files_rejects_entries_that_escape_pon() {
        let layout = EnvLayout::new(temp_project("record-escape"));
        fs::create_dir_all(layout.site_packages.join("demo-1.0.dist-info")).expect("dist-info");
        fs::write(
            layout.site_packages.join("demo-1.0.dist-info/RECORD"),
            "../../../outside,,\n",
        )
        .expect("record");

        let error = remove_record_files(&layout, Path::new("demo-1.0.dist-info/RECORD"))
            .expect_err("escape");

        let Error::UnsupportedArtifact(message) = error else {
            panic!("expected UnsupportedArtifact");
        };
        assert!(message.contains("unsafe RECORD entry"));
    }

    #[test]
    fn remove_without_registry_record_uses_matching_dist_info_record() {
        let layout = EnvLayout::new(temp_project("legacy-record-scan"));
        fs::create_dir_all(layout.site_packages.join("demo")).expect("package");
        fs::create_dir_all(layout.site_packages.join("demo-1.0.dist-info")).expect("dist-info");
        fs::write(layout.site_packages.join("demo/__init__.py"), "").expect("module");
        fs::write(
            layout.site_packages.join("demo-1.0.dist-info/RECORD"),
            "demo/__init__.py,,\ndemo-1.0.dist-info/RECORD,,\n",
        )
        .expect("record");
        write_installed_packages(
            &layout,
            &[InstalledPackageRecord {
                name: "demo".to_owned(),
                version: "1.0".to_owned(),
                artifact_kind: "wheel".to_owned(),
                import_names: vec!["demo".to_owned()],
                record_path: None,
            }],
        )
        .expect("registry");

        let removed = remove_installed_package(&layout, "demo").expect("remove");

        assert_eq!(removed.expect("removed").name, "demo");
        assert!(!layout.site_packages.join("demo/__init__.py").exists());
        assert!(read_installed_packages(&layout).expect("registry").is_empty());
    }

    #[test]
    fn install_package_same_version_returns_existing_record_without_reinstalling() {
        let layout = EnvLayout::new(temp_project("install-idempotent"));
        write_installed_packages(
            &layout,
            &[InstalledPackageRecord {
                name: "idna".to_owned(),
                version: "3.10".to_owned(),
                artifact_kind: "wheel".to_owned(),
                import_names: vec!["idna".to_owned()],
                record_path: None,
            }],
        )
        .expect("registry");
        let record = ResolvedRecord::wheel("idna", "3.10", "missing.whl");

        let report = install_package(&layout, &record).expect("idempotent");

        assert_eq!(report.package_name, "idna");
        assert_eq!(report.version, "3.10");
        assert_eq!(report.artifact_kind, "wheel");
        assert_eq!(report.import_names, vec!["idna".to_owned()]);
    }

    #[test]
    fn editable_install_links_source_and_uninstall_removes_only_link() {
        let layout = EnvLayout::new(temp_project("editable-env"));
        let source = temp_project("editable-src");
        let package_root = source.join("src/demo_pkg");
        fs::create_dir_all(&package_root).expect("package root");
        fs::write(
            source.join("pyproject.toml"),
            "[project]\nname = \"demo-pkg\"\nversion = \"1.0\"\n",
        )
        .expect("pyproject");
        fs::write(package_root.join("__init__.py"), "VALUE = 1\n").expect("source module");
        let record = ResolvedRecord::dir("demo-pkg", "1.0", &source, true);

        let report = install_package(&layout, &record).expect("editable install");

        assert_eq!(report.package_name, "demo-pkg");
        assert_eq!(report.artifact_kind, "editable");
        assert_eq!(report.import_names, vec!["demo_pkg".to_owned()]);
        let installed_import = layout.site_packages.join("demo_pkg");
        #[cfg(unix)]
        assert!(
            fs::symlink_metadata(&installed_import)
                .expect("installed import metadata")
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_to_string(installed_import.join("__init__.py")).expect("linked source"),
            "VALUE = 1\n"
        );

        let removed = remove_installed_package(&layout, "demo-pkg").expect("remove editable");

        assert_eq!(removed.expect("removed").artifact_kind, "editable");
        assert!(!installed_import.exists());
        assert_eq!(
            fs::read_to_string(package_root.join("__init__.py")).expect("source intact"),
            "VALUE = 1\n"
        );
    }

    fn temp_project(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("pon-pm-install-{label}-{unique}"))
    }
}
