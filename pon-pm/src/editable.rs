//! Editable installation support for local Python source trees.

use std::fs;
use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::env::EnvLayout;
use crate::error::{Error, Result};
use crate::install::{InstallReport, InstalledPackageRecord, ResolvedRecord, upsert_installed_package};
use crate::names;
use crate::pyproject::PyProject;

struct EditableManifest {
    package_name: String,
    normalized_name: String,
    version: String,
    import_name: String,
    dependencies: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceKind {
    PackageDir,
    ModuleFile,
}

struct SourceRoot {
    path: PathBuf,
    relative_import_path: PathBuf,
    kind: SourceKind,
}

struct RecordFile {
    path: String,
    hash: Option<String>,
    size: Option<u64>,
}

/// Install `path` as an editable package by linking its import root into `site-packages`.
///
/// The import root is selected from `[tool.pon].import-name`, or from the normalized
/// project name with `-` replaced by `_` when the setting is absent. Resolution checks
/// `src/<import>`, `<import>`, and module-file variants in that order. The installer
/// writes normal dist-info metadata and a registry row; it never creates `.pth` files.
/// On Windows, where symlink creation can require privileges, a failed symlink attempt
/// falls back to copying the source root and emits a warning.
pub fn install_editable(env: &EnvLayout, resolved_record: &ResolvedRecord, path: &Path) -> Result<InstallReport> {
    let manifest = read_manifest(&path.join("pyproject.toml"))?;
    validate_resolved_record(&manifest, resolved_record, path)?;

    let source = package_source_path(path, &manifest.import_name)?;
    let destination_relative = destination_relative_path(&source);
    let destination = env.site_packages.join(&destination_relative);
    let dist_info_dir = format!("{}-{}.dist-info", manifest.normalized_name, manifest.version);
    let dist_info_relative = PathBuf::from(&dist_info_dir);
    let dist_info_path = env.site_packages.join(&dist_info_relative);
    let record_relative = dist_info_relative.join("RECORD");

    env.create_dirs()?;
    remove_existing_path(&destination)?;
    remove_existing_path(&dist_info_path)?;

    let mut record_files = install_source_root(&source, &destination, &destination_relative)?;

    fs::create_dir_all(&dist_info_path)?;
    record_files.push(write_dist_info_file(
        &dist_info_path.join("METADATA"),
        dist_info_relative.join("METADATA"),
        render_metadata(&manifest),
    )?);
    record_files.push(write_dist_info_file(
        &dist_info_path.join("INSTALLER"),
        dist_info_relative.join("INSTALLER"),
        "pon-pm\n".to_owned(),
    )?);
    record_files.push(write_dist_info_file(
        &dist_info_path.join("direct_url.json"),
        dist_info_relative.join("direct_url.json"),
        render_direct_url(path)?,
    )?);
    record_files.push(RecordFile {
        path: record_path_string(&record_relative),
        hash: None,
        size: None,
    });

    fs::write(env.site_packages.join(&record_relative), render_record(&record_files))?;

    upsert_installed_package(
        env,
        InstalledPackageRecord {
            name: manifest.normalized_name.clone(),
            version: manifest.version.clone(),
            artifact_kind: "editable".to_owned(),
            import_names: vec![manifest.import_name.clone()],
            record_path: Some(record_relative),
        },
    )?;

    Ok(InstallReport {
        package_name: manifest.normalized_name,
        version: manifest.version,
        artifact_kind: "editable".to_owned(),
        import_names: vec![manifest.import_name],
    })
}

fn read_manifest(path: &Path) -> Result<EditableManifest> {
    let pyproject = PyProject::read(path)?;
    let package_name = pyproject
        .project_name()
        .ok_or_else(|| Error::UnsupportedArtifact(format!("{} is missing [project].name", path.display())))?
        .to_owned();
    let version = pyproject
        .project_version()
        .ok_or_else(|| Error::UnsupportedArtifact(format!("{} is missing [project].version", path.display())))?
        .to_owned();
    let normalized_name = names::normalize(&package_name);
    let import_name = pyproject
        .tool_pon_import_name()
        .map(str::to_owned)
        .unwrap_or_else(|| normalized_name.replace('-', "_"));
    if !is_import_path(&import_name) {
        return Err(Error::UnsupportedArtifact(format!(
            "editable import name `{import_name}` must be a dotted Python import path"
        )));
    }

    Ok(EditableManifest {
        package_name,
        normalized_name,
        version,
        import_name,
        dependencies: pyproject.dependencies(),
    })
}

fn validate_resolved_record(manifest: &EditableManifest, resolved_record: &ResolvedRecord, path: &Path) -> Result<()> {
    let normalized_record_name = resolved_record.normalized_name();
    if manifest.normalized_name != normalized_record_name {
        return Err(Error::UnsupportedArtifact(format!(
            "editable package `{}` does not match resolved package `{}`",
            manifest.normalized_name, normalized_record_name
        )));
    }
    if manifest.version != resolved_record.version {
        return Err(Error::UnsupportedArtifact(format!(
            "editable package `{}` version `{}` does not match resolved version `{}`",
            path.display(),
            manifest.version,
            resolved_record.version
        )));
    }
    Ok(())
}

fn package_source_path(root: &Path, import_name: &str) -> Result<SourceRoot> {
    let relative = import_name.split('.').collect::<PathBuf>();
    for base in [root.join("src"), root.to_path_buf()] {
        let package_dir = base.join(&relative);
        if package_dir.join("__init__.py").is_file() {
            return Ok(SourceRoot {
                path: package_dir,
                relative_import_path: relative.clone(),
                kind: SourceKind::PackageDir,
            });
        }

        let module_file = base.join(&relative).with_extension("py");
        if module_file.is_file() {
            return Ok(SourceRoot {
                path: module_file,
                relative_import_path: relative.clone(),
                kind: SourceKind::ModuleFile,
            });
        }
    }

    Err(Error::UnsupportedArtifact(format!(
        "editable package `{}` does not contain import `{import_name}` under package root or src/",
        root.display()
    )))
}

fn destination_relative_path(source: &SourceRoot) -> PathBuf {
    match source.kind {
        SourceKind::PackageDir => source.relative_import_path.clone(),
        SourceKind::ModuleFile => source.relative_import_path.with_extension("py"),
    }
}

fn install_source_root(source: &SourceRoot, destination: &Path, destination_relative: &Path) -> Result<Vec<RecordFile>> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }

    let link_source = absolute_path(&source.path)?;
    match create_editable_link(&link_source, destination, source.kind)? {
        LinkOutcome::Linked => Ok(vec![RecordFile {
            path: record_path_string(destination_relative),
            hash: None,
            size: None,
        }]),
        LinkOutcome::Copied => {
            let mut files = Vec::new();
            match source.kind {
                SourceKind::PackageDir => copy_tree(&source.path, destination, destination_relative, &mut files)?,
                SourceKind::ModuleFile => {
                    copy_file(&source.path, destination)?;
                    files.push(record_file_from_path(destination, destination_relative)?);
                }
            }
            Ok(files)
        }
    }
}

enum LinkOutcome {
    Linked,
    #[cfg_attr(unix, allow(dead_code, reason = "constructed only by the windows and fallback create_editable_link impls"))]
    Copied,
}

#[cfg(unix)]
fn create_editable_link(source: &Path, destination: &Path, _kind: SourceKind) -> Result<LinkOutcome> {
    std::os::unix::fs::symlink(source, destination)?;
    Ok(LinkOutcome::Linked)
}

#[cfg(windows)]
fn create_editable_link(source: &Path, destination: &Path, kind: SourceKind) -> Result<LinkOutcome> {
    let result = match kind {
        SourceKind::PackageDir => std::os::windows::fs::symlink_dir(source, destination),
        SourceKind::ModuleFile => std::os::windows::fs::symlink_file(source, destination),
    };
    match result {
        Ok(()) => Ok(LinkOutcome::Linked),
        Err(error) => {
            eprintln!(
                "warning: editable install copied files (symlinks unavailable: {error}); re-run `pon-pm install` after edits"
            );
            Ok(LinkOutcome::Copied)
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn create_editable_link(_source: &Path, _destination: &Path, _kind: SourceKind) -> Result<LinkOutcome> {
    Ok(LinkOutcome::Copied)
}

fn copy_tree(source: &Path, destination: &Path, record_prefix: &Path, record_files: &mut Vec<RecordFile>) -> Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let file_name = entry.file_name();
        if file_name == "__pycache__" {
            continue;
        }

        let source_path = entry.path();
        let destination_path = destination.join(&file_name);
        let record_path = record_prefix.join(&file_name);
        if source_path.is_dir() {
            copy_tree(&source_path, &destination_path, &record_path, record_files)?;
        } else if source_path.is_file() {
            copy_file(&source_path, &destination_path)?;
            record_files.push(record_file_from_path(&destination_path, &record_path)?);
        }
    }
    Ok(())
}

fn copy_file(source: &Path, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(source, destination)?;
    Ok(())
}

fn remove_existing_path(path: &Path) -> Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() || metadata.is_file() {
        fs::remove_file(path)?;
    } else if metadata.is_dir() {
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

fn write_dist_info_file(path: &Path, relative_path: PathBuf, content: String) -> Result<RecordFile> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)?;
    record_file_from_path(path, &relative_path)
}

fn render_metadata(manifest: &EditableManifest) -> String {
    let mut metadata = format!(
        "Metadata-Version: 2.1\nName: {}\nVersion: {}\n",
        manifest.package_name, manifest.version
    );
    for dependency in &manifest.dependencies {
        metadata.push_str("Requires-Dist: ");
        metadata.push_str(dependency);
        metadata.push('\n');
    }
    metadata.push('\n');
    metadata
}

fn render_direct_url(path: &Path) -> Result<String> {
    let absolute = absolute_path(path)?;
    let value = json!({
        "url": file_url(&absolute),
        "dir_info": { "editable": true },
    });
    Ok(format!("{}\n", value))
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()?.join(path))
}

fn file_url(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    let with_leading_slash = if raw.starts_with('/') {
        raw
    } else {
        format!("/{raw}")
    };
    format!("file://{}", percent_encode_path(&with_leading_slash))
}

fn percent_encode_path(path: &str) -> String {
    let mut encoded = String::new();
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn record_file_from_path(path: &Path, relative_path: &Path) -> Result<RecordFile> {
    let bytes = fs::read(path)?;
    let digest = Sha256::digest(&bytes);
    Ok(RecordFile {
        path: record_path_string(relative_path),
        hash: Some(format!("sha256={}", URL_SAFE_NO_PAD.encode(digest))),
        size: Some(bytes.len() as u64),
    })
}

fn render_record(files: &[RecordFile]) -> String {
    let mut out = String::new();
    for file in files {
        out.push_str(&csv_field(&file.path));
        out.push(',');
        if let Some(hash) = &file.hash {
            out.push_str(hash);
        }
        out.push(',');
        if let Some(size) = file.size {
            out.push_str(&size.to_string());
        }
        out.push('\n');
    }
    out
}

fn record_path_string(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn is_import_path(value: &str) -> bool {
    !value.is_empty() && value.split('.').all(is_identifier)
}

fn is_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic()) && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}
