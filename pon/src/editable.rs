//! Editable installation support for local Python source trees.

use std::{
	fs,
	path::{Path, PathBuf},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
	env::EnvLayout,
	error::{Error, Result},
	install::{InstallReport, InstalledPackageRecord, ResolvedRecord, upsert_installed_package},
	names,
	pyproject::PyProject,
};

struct EditableManifest {
	package_name:    String,
	normalized_name: String,
	version:         String,
	import_name:     String,
	dependencies:    Vec<String>,
}

struct SourceRoot {
	/// Directory that must be placed on `sys.path` for the editable import to
	/// resolve (`project/src` or the project root).
	import_base:          PathBuf,
	/// Legacy symlink/copy location from the pre-.pth installer, removed on
	/// reinstall so the .pth convention is the only live import path.
	legacy_relative_path: PathBuf,
}

struct RecordFile {
	path: String,
	hash: Option<String>,
	size: Option<u64>,
}

/// Install `path` as an editable package by writing a standard `.pth` file
/// into `site-packages`.
///
/// The import root is selected from `[tool.pon].import-name`, or from the
/// normalized project name with `-` replaced by `_` when the setting is absent.
/// Resolution checks `src/<import>`, `<import>`, and module-file variants in
/// that order. The installer writes normal dist-info metadata, a PEP 610
/// `direct_url.json`, a RECORD that includes the `.pth`, and a registry row.
pub fn install_editable(
	env: &EnvLayout,
	resolved_record: &ResolvedRecord,
	path: &Path,
) -> Result<InstallReport> {
	let manifest = read_manifest(&path.join("pyproject.toml"))?;
	validate_resolved_record(&manifest, resolved_record, path)?;

	let source = package_source_path(path, &manifest.import_name)?;
	let legacy_destination = env.site_packages.join(&source.legacy_relative_path);
	let pth_relative = editable_pth_relative_path(&manifest);
	let pth_path = env.site_packages.join(&pth_relative);
	let dist_info_dir = format!("{}-{}.dist-info", manifest.normalized_name, manifest.version);
	let dist_info_relative = PathBuf::from(&dist_info_dir);
	let dist_info_path = env.site_packages.join(&dist_info_relative);
	let record_relative = dist_info_relative.join("RECORD");

	env.create_dirs()?;
	remove_existing_path(&legacy_destination)?;
	remove_existing_path(&pth_path)?;
	remove_existing_path(&dist_info_path)?;

	let mut record_files = vec![write_editable_pth(&pth_path, &pth_relative, &source)?];

	fs::create_dir_all(&dist_info_path)?;
	record_files.push(write_dist_info_file(
		&dist_info_path.join("METADATA"),
		dist_info_relative.join("METADATA"),
		render_metadata(&manifest),
	)?);
	record_files.push(write_dist_info_file(
		&dist_info_path.join("INSTALLER"),
		dist_info_relative.join("INSTALLER"),
		"pon\n".to_owned(),
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

	upsert_installed_package(env, InstalledPackageRecord {
		name:          manifest.normalized_name.clone(),
		version:       manifest.version.clone(),
		artifact_kind: "editable".to_owned(),
		import_names:  vec![manifest.import_name.clone()],
		record_path:   Some(record_relative),
	})?;

	Ok(InstallReport {
		package_name:  manifest.normalized_name,
		version:       manifest.version,
		artifact_kind: "editable".to_owned(),
		import_names:  vec![manifest.import_name],
	})
}

fn read_manifest(path: &Path) -> Result<EditableManifest> {
	let pyproject = PyProject::read(path)?;
	let package_name = pyproject
		.project_name()
		.ok_or_else(|| {
			Error::UnsupportedArtifact(format!("{} is missing [project].name", path.display()))
		})?
		.to_owned();
	let version = pyproject
		.project_version()
		.ok_or_else(|| {
			Error::UnsupportedArtifact(format!("{} is missing [project].version", path.display()))
		})?
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

fn validate_resolved_record(
	manifest: &EditableManifest,
	resolved_record: &ResolvedRecord,
	path: &Path,
) -> Result<()> {
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
				import_base:          base,
				legacy_relative_path: relative.clone(),
			});
		}

		let module_file = base.join(&relative).with_extension("py");
		if module_file.is_file() {
			return Ok(SourceRoot {
				import_base:          base,
				legacy_relative_path: relative.with_extension("py"),
			});
		}
	}

	Err(Error::UnsupportedArtifact(format!(
		"editable package `{}` does not contain import `{import_name}` under package root or src/",
		root.display()
	)))
}

fn editable_pth_relative_path(manifest: &EditableManifest) -> PathBuf {
	PathBuf::from(format!("{}-editable.pth", manifest.normalized_name))
}

fn write_editable_pth(
	path: &Path,
	relative_path: &Path,
	source: &SourceRoot,
) -> Result<RecordFile> {
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent)?;
	}
	let import_base = absolute_path(&source.import_base)?;
	fs::write(path, format!("{}\n", import_base.display()))?;
	record_file_from_path(path, relative_path)
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

fn write_dist_info_file(
	path: &Path,
	relative_path: PathBuf,
	content: String,
) -> Result<RecordFile> {
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
			},
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
	path
		.components()
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
	(first == '_' || first.is_ascii_alphabetic())
		&& chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}
