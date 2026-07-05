use std::{
	fs,
	path::{Path, PathBuf},
};

use crate::{
	env::EnvLayout,
	error::{Error, Result},
	install::{InstallReport, InstalledPackageRecord, ResolvedRecord, upsert_installed_package},
	names,
	pyproject::PyProject,
};

struct LocalPythonManifest {
	package_name: String,
	version:      String,
	import_name:  String,
}

pub fn install_local_python_package(
	env: &EnvLayout,
	resolved_record: &ResolvedRecord,
	path: &Path,
) -> Result<InstallReport> {
	let manifest_path = path.join("pyproject.toml");
	let manifest = read_manifest(&manifest_path)?;
	let normalized_manifest_name = names::normalize(&manifest.package_name);
	let normalized_record_name = resolved_record.normalized_name();
	if normalized_manifest_name != normalized_record_name {
		return Err(Error::UnsupportedArtifact(format!(
			"local package `{}` does not match resolved package `{}`",
			normalized_manifest_name, normalized_record_name
		)));
	}
	if manifest.version != resolved_record.version {
		return Err(Error::UnsupportedArtifact(format!(
			"local package `{}` version `{}` does not match resolved version `{}`",
			manifest.package_name, manifest.version, resolved_record.version
		)));
	}

	let source = package_source_path(path, &manifest.import_name)?;
	let dist_info = format!("{}-{}.dist-info", manifest.import_name, manifest.version);
	let record_path = PathBuf::from(&dist_info).join("RECORD");
	let mut record_entries = Vec::new();

	env.create_dirs()?;
	if source.is_dir() {
		copy_tree(
			&source,
			&env.site_packages.join(&manifest.import_name),
			Path::new(&manifest.import_name),
			&mut record_entries,
		)?;
	} else {
		let destination_name = format!("{}.py", manifest.import_name);
		copy_file(&source, &env.site_packages.join(&destination_name))?;
		record_entries.push(destination_name);
	}

	let record_relative = record_path.to_string_lossy().into_owned();
	record_entries.push(record_relative.clone());
	let mut record_text = String::new();
	for entry in &record_entries {
		record_text.push_str(entry);
		record_text.push_str(",,\n");
	}
	let record_full_path = env.site_packages.join(&record_path);
	if let Some(parent) = record_full_path.parent() {
		fs::create_dir_all(parent)?;
	}
	fs::write(&record_full_path, record_text)?;

	upsert_installed_package(env, InstalledPackageRecord {
		name:          normalized_manifest_name.clone(),
		version:       manifest.version.clone(),
		artifact_kind: "local".to_owned(),
		import_names:  vec![manifest.import_name.clone()],
		record_path:   Some(record_path),
	})?;
	Ok(InstallReport {
		package_name:  normalized_manifest_name,
		version:       manifest.version,
		artifact_kind: "local".to_owned(),
		import_names:  vec![manifest.import_name],
	})
}

fn read_manifest(path: &Path) -> Result<LocalPythonManifest> {
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
	let import_name = pyproject
		.tool_pon_import_name()
		.map(str::to_owned)
		.unwrap_or_else(|| names::normalize(&package_name).replace('-', "_"));
	if !is_import_path(&import_name) {
		return Err(Error::UnsupportedArtifact(format!(
			"local import name `{import_name}` must be a dotted Python import path"
		)));
	}
	Ok(LocalPythonManifest { package_name, version, import_name })
}

fn package_source_path(root: &Path, import_name: &str) -> Result<PathBuf> {
	let relative = import_name.split('.').collect::<PathBuf>();
	for base in [root.join("src"), root.to_path_buf()] {
		let package_dir = base.join(&relative);
		if package_dir.join("__init__.py").is_file() {
			return Ok(package_dir);
		}
		let module_file = base.join(&relative).with_extension("py");
		if module_file.is_file() {
			return Ok(module_file);
		}
	}
	Err(Error::UnsupportedArtifact(format!(
		"local package `{}` does not contain import `{import_name}` under package root or src/",
		root.display()
	)))
}

fn copy_tree(
	source: &Path,
	destination: &Path,
	record_prefix: &Path,
	record_entries: &mut Vec<String>,
) -> Result<()> {
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
			copy_tree(&source_path, &destination_path, &record_path, record_entries)?;
		} else if source_path.is_file() {
			copy_file(&source_path, &destination_path)?;
			record_entries.push(record_path.to_string_lossy().into_owned());
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
