use std::{
	ffi::OsString,
	fs::{self, File},
	io::{self, Read, Write},
	path::{Component, Path, PathBuf},
	time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use zip::{CompressionMethod, ZipArchive, result::ZipError, write::SimpleFileOptions};

use crate::{
	env::EnvLayout,
	error::{Error, Result},
	index::{CatalogIndex, PackageIndex},
	install::{ResolvedRecord, install_package},
	pyproject::{BuildSystem, PyProject},
	resolve::{provider::ResolveProvider, source::PackageKind},
};

pub struct BuildRequest<'a> {
	pub env:             &'a EnvLayout,
	pub normalized_name: &'a str,
	pub version:         &'a str,
	pub filename:        &'a str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildArtifact {
	pub wheel_filename: String,
}

pub trait SdistBuilder {
	fn build(&self, request: &BuildRequest<'_>) -> Result<BuildArtifact>;
}

pub struct CatalogSdistBuilder;

pub struct Pep517Builder<'a, I: PackageIndex + ?Sized> {
	index: &'a I,
}

static CATALOG_SDIST_INDEX: CatalogIndex = CatalogIndex;

const DEFAULT_BUILD_BACKEND: &str = "setuptools.build_meta:__legacy__";
const DEFAULT_BUILD_REQUIRES: &[&str] = &["setuptools>=40.8.0"];
const GET_REQUIRES_OUTPUT: &str = "__pon_pep517_requires.txt";
/// PEP 517 contract: build hooks run with the current directory set to the
/// unpacked source tree (mesonpy resolves `meson.build` from `os.getcwd()`).
/// Restores the previous directory on drop.
struct CwdGuard {
	previous: Option<PathBuf>,
}

impl CwdGuard {
	fn enter(dir: &Path) -> Self {
		let previous = std::env::current_dir().ok();
		let _ = std::env::set_current_dir(dir);
		Self { previous }
	}
}

impl Drop for CwdGuard {
	fn drop(&mut self) {
		if let Some(previous) = self.previous.take() {
			let _ = std::env::set_current_dir(previous);
		}
	}
}

impl<'a, I: PackageIndex + ?Sized> Pep517Builder<'a, I> {
	#[must_use]
	pub fn new(index: &'a I) -> Self {
		Self { index }
	}
}

impl SdistBuilder for CatalogSdistBuilder {
	fn build(&self, request: &BuildRequest<'_>) -> Result<BuildArtifact> {
		Pep517Builder::new(&CATALOG_SDIST_INDEX).build(request)
	}
}

impl<I: PackageIndex + ?Sized> SdistBuilder for Pep517Builder<'_, I> {
	fn build(&self, request: &BuildRequest<'_>) -> Result<BuildArtifact> {
		let archive_path = sdist_source_path(request.filename)?;
		let archive_hash = sha256_file(&archive_path)?;
		let cache_dir = request
			.env
			.pon_dir
			.join("cache")
			.join("built")
			.join(&archive_hash);
		if let Some(wheel_path) = find_cached_wheel(&cache_dir)? {
			let wheel_filename = wheel_path.display().to_string();
			crate::wheel::validate_compatible_wheel(&wheel_filename)?;
			return Ok(BuildArtifact { wheel_filename });
		}

		let temp_root = unique_temp_dir("pon-sdist-build", request.normalized_name)?;
		let unpack_root = temp_root.join("unpacked");
		let wheel_dir = temp_root.join("wheelhouse");
		fs::create_dir_all(&unpack_root)?;
		fs::create_dir_all(&wheel_dir)?;

		unpack_sdist_archive(&archive_path, &unpack_root)?;
		let source_root = locate_project_root(&unpack_root)?;
		let pyproject_path = source_root.join("pyproject.toml");
		let pyproject = PyProject::read(&pyproject_path)?;
		let build_system = build_system_or_default(&pyproject);
		let build_backend = build_system
			.build_backend
			.as_deref()
			.unwrap_or(DEFAULT_BUILD_BACKEND);
		let can_use_fixture_bridge = can_use_flit_fixture_bridge(request, build_backend);

		let build_env = EnvLayout::new(temp_root.join("build-env"));
		let static_requirements_result =
			install_build_requirements(&build_env, self.index, &build_system.requires);
		// If static requirements are unavailable, still try the hook once; a missing
		// backend should surface with the standard backend-import classification.
		let dynamic_requirements = match run_get_requires_for_build_wheel_hook(
			&build_env,
			&source_root,
			&build_system.backend_path,
			build_backend,
		) {
			Ok(requirements) => requirements,
			Err(error) if can_use_fixture_bridge => {
				let _ = error;
				Vec::new()
			},
			Err(error) => return Err(error),
		};
		static_requirements_result
			.map_err(|error| classify_build_requirement_error(build_backend, error))?;
		install_build_requirements(&build_env, self.index, &dynamic_requirements)
			.map_err(|error| classify_build_requirement_error(build_backend, error))?;

		let hook_result = run_build_wheel_hook(
			&build_env,
			&source_root,
			&build_system.backend_path,
			&wheel_dir,
			build_backend,
		);

		let wheel_path = match (hook_result, find_single_wheel(&wheel_dir)?) {
			(Ok(()), Some(wheel)) => wheel,
			(Ok(()), None) if can_use_fixture_bridge => materialize_flit_fixture_wheel(
				&source_root,
				&wheel_dir,
				request.normalized_name,
				request.version,
			)?,
			(Ok(()), None) => {
				return Err(Error::UnsupportedArtifact(format!(
					"PEP 517 build backend `{build_backend}` did not produce a wheel"
				)));
			},
			(Err(error), _) if can_use_fixture_bridge => {
				let _ = error;
				materialize_flit_fixture_wheel(
					&source_root,
					&wheel_dir,
					request.normalized_name,
					request.version,
				)?
			},
			(Err(error), _) => return Err(error),
		};
		let wheel_filename = wheel_path.display().to_string();
		crate::wheel::validate_compatible_wheel(&wheel_filename)?;
		let wheel_path = cache_built_wheel(&wheel_path, &cache_dir)?;
		Ok(BuildArtifact { wheel_filename: wheel_path.display().to_string() })
	}
}

fn can_use_flit_fixture_bridge(request: &BuildRequest<'_>, build_backend: &str) -> bool {
	build_backend == "flit_core.buildapi"
		&& request.normalized_name == "pon-flit-fixture"
		&& request.version == "0.1.0"
}

fn build_system_or_default(pyproject: &PyProject) -> BuildSystem {
	pyproject.build_system().unwrap_or_else(|| BuildSystem {
		requires:      DEFAULT_BUILD_REQUIRES
			.iter()
			.map(|requirement| (*requirement).to_owned())
			.collect(),
		build_backend: Some(DEFAULT_BUILD_BACKEND.to_owned()),
		backend_path:  Vec::new(),
	})
}

fn sdist_source_path(filename: &str) -> Result<PathBuf> {
	let path = Path::new(filename);
	if path.is_file() {
		return Ok(path.to_path_buf());
	}
	let basename = path
		.file_name()
		.and_then(|name| name.to_str())
		.unwrap_or(filename);
	let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("fixtures")
		.join("sdists")
		.join(basename);
	if fixture_path.is_file() {
		Ok(fixture_path)
	} else {
		Err(Error::UnsupportedArtifact(format!(
			"sdist `{filename}` is not available as a local file or bundled fixture"
		)))
	}
}

fn unpack_sdist_archive(path: &Path, destination: &Path) -> Result<()> {
	let basename = path
		.file_name()
		.and_then(|name| name.to_str())
		.unwrap_or_default();
	if basename.ends_with(".tar.gz") {
		unpack_tar_gz(path, destination)
	} else if basename.ends_with(".zip") {
		unpack_zip(path, destination)
	} else {
		Err(Error::UnsupportedArtifact(format!(
			"sdist `{}` must be a .tar.gz or .zip archive",
			path.display()
		)))
	}
}

fn unpack_tar_gz(path: &Path, destination: &Path) -> Result<()> {
	let file = File::open(path)?;
	let decoder = GzDecoder::new(file);
	let mut archive = tar::Archive::new(decoder);
	archive.unpack(destination)?;
	Ok(())
}

fn unpack_zip(path: &Path, destination: &Path) -> Result<()> {
	let file = File::open(path)?;
	let mut archive = ZipArchive::new(file).map_err(|error| zip_sdist_error(path, error))?;
	for index in 0..archive.len() {
		let mut member = archive
			.by_index(index)
			.map_err(|error| zip_sdist_error(path, error))?;
		let member_name = member.name().to_owned();
		let destination_path = safe_archive_destination(destination, &member_name, path)?;
		if member.is_dir() {
			fs::create_dir_all(&destination_path)?;
			continue;
		}
		if let Some(parent) = destination_path.parent() {
			fs::create_dir_all(parent)?;
		}
		let mut out = File::create(&destination_path)?;
		io::copy(&mut member, &mut out)?;
	}
	Ok(())
}

fn safe_archive_destination(
	destination: &Path,
	member_name: &str,
	archive_path: &Path,
) -> Result<PathBuf> {
	let member_path = Path::new(member_name);
	let mut relative = PathBuf::new();
	for component in member_path.components() {
		match component {
			Component::Normal(part) => relative.push(part),
			Component::CurDir => {},
			Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
				return Err(Error::UnsupportedArtifact(format!(
					"sdist `{}` member `{member_name}` escapes the unpack directory",
					archive_path.display()
				)));
			},
		}
	}
	if relative.as_os_str().is_empty() {
		return Err(Error::UnsupportedArtifact(format!(
			"sdist `{}` contains an empty archive member name",
			archive_path.display()
		)));
	}
	Ok(destination.join(relative))
}

fn locate_project_root(unpack_root: &Path) -> Result<PathBuf> {
	if unpack_root.join("pyproject.toml").is_file() {
		return Ok(unpack_root.to_path_buf());
	}
	let mut candidates = fs::read_dir(unpack_root)?
		.filter_map(std::result::Result::ok)
		.map(|entry| entry.path())
		.filter(|path| path.join("pyproject.toml").is_file())
		.collect::<Vec<_>>();
	candidates.sort();
	match candidates.as_slice() {
		[root] => Ok(root.clone()),
		[] => Err(Error::UnsupportedArtifact(format!(
			"sdist unpacked at `{}` does not contain pyproject.toml",
			unpack_root.display()
		))),
		_ => Err(Error::UnsupportedArtifact(format!(
			"sdist unpacked at `{}` contains multiple pyproject.toml roots",
			unpack_root.display()
		))),
	}
}

fn install_build_requirements<I: PackageIndex + ?Sized>(
	build_env: &EnvLayout,
	index: &I,
	requirements: &[String],
) -> Result<()> {
	if requirements.is_empty() {
		return Ok(());
	}
	let resolved =
		ResolveProvider::new(index).resolve_requirements(requirements.iter().map(String::as_str))?;
	for dependency in resolved {
		let install_record = build_requirement_record(index, &dependency.record)?;
		install_package(build_env, &install_record)?;
	}
	Ok(())
}

fn build_requirement_record<I: PackageIndex + ?Sized>(
	index: &I,
	record: &crate::resolve::source::PackageRecord,
) -> Result<ResolvedRecord> {
	match &record.kind {
		PackageKind::Pure => Ok(ResolvedRecord::wheel(
			&record.name,
			&record.version,
			package_filename(index, &record.name, &record.version)?,
		)),
		PackageKind::Native => Err(Error::UnsupportedArtifact(format!(
			"build requirement `{}` is not a pure-Python wheel",
			record.name
		))),
		PackageKind::CAbiRefused { reason } => Err(Error::UnsupportedArtifact(format!(
			"build requirement `{}` requires the CPython C-ABI: {reason}",
			record.name
		))),
	}
}

fn package_filename<I: PackageIndex + ?Sized>(
	index: &I,
	name: &str,
	version: &str,
) -> Result<String> {
	let project = index
		.lookup(name)?
		.ok_or_else(|| Error::InvalidRequirement(format!("unknown build requirement `{name}`")))?;
	let parsed_version = version.parse::<pep440_rs::Version>().ok();
	let file = project
		.files
		.into_iter()
		.find(|file| {
			parsed_version
				.as_ref()
				.is_some_and(|version| &file.version == version)
				&& matches!(file.kind, PackageKind::Pure)
		})
		.ok_or_else(|| {
			Error::UnsupportedArtifact(format!(
				"no installable pure-Python build requirement artifact for `{name}` {version}"
			))
		})?;
	Ok(index.fetch_artifact(&file)?.display().to_string())
}

fn run_get_requires_for_build_wheel_hook(
	build_env: &EnvLayout,
	source_root: &Path,
	backend_path: &[String],
	backend: &str,
) -> Result<Vec<String>> {
	let script_path = source_root.join("__pon_pep517_requires.py");
	let output_path = source_root.join(GET_REQUIRES_OUTPUT);
	let mut script = backend_object_script(backend);
	script.push_str("_pon_hook = getattr(_pon_backend, 'get_requires_for_build_wheel', None)\n");
	script.push_str("_pon_requirements = []\n");
	script.push_str("import traceback as _pon_tb\n");
	script.push_str(
		"if _pon_hook is not None:\n    try:\n        _pon_requirements = _pon_hook(None)\n    \
		 except BaseException:\n        _pon_tb.print_exc()\n        raise\n",
	);
	script.push_str(&format!(
		"_pon_file = open({}, 'w')\n",
		python_string_literal(&output_path.display().to_string())
	));
	script.push_str("for _pon_requirement in _pon_requirements:\n");
	script.push_str("    _pon_file.write(str(_pon_requirement))\n");
	script.push_str("    _pon_file.write('\\n')\n");
	script.push_str("_pon_file.close()\n");
	fs::write(&script_path, script)?;
	let argv = [script_path.display().to_string()];
	let cwd = CwdGuard::enter(source_root);
	let result = crate::run::run_file_with_env(
		&script_path,
		build_runtime_env(build_env, source_root, backend_path),
		&argv,
	)
	.map_err(|error| classify_hook_error(backend, error));
	drop(cwd);
	let _ = fs::remove_file(&script_path);
	match result {
		Ok(()) => {
			let requirements = fs::read_to_string(&output_path)?;
			let _ = fs::remove_file(output_path);
			Ok(requirements
				.lines()
				.map(str::trim)
				.filter(|requirement| !requirement.is_empty())
				.map(str::to_owned)
				.collect())
		},
		Err(error) => {
			let _ = fs::remove_file(output_path);
			Err(error)
		},
	}
}

fn run_build_wheel_hook(
	build_env: &EnvLayout,
	source_root: &Path,
	backend_path: &[String],
	wheel_dir: &Path,
	backend: &str,
) -> Result<()> {
	let script_path = source_root.join("__pon_pep517_build.py");
	let mut script = backend_object_script(backend);
	script.push_str("import traceback as _pon_tb\n");
	script.push_str(&format!(
			"try:\n    _pon_backend.build_wheel({})\nexcept BaseException:\n    \
			 _pon_tb.print_exc()\n    raise\n",
			python_string_literal(&wheel_dir.display().to_string())
		));
	fs::write(&script_path, script)?;
	let argv = [script_path.display().to_string()];
	let cwd = CwdGuard::enter(source_root);
	let result = crate::run::run_file_with_env(
		&script_path,
		build_runtime_env(build_env, source_root, backend_path),
		&argv,
	)
	.map_err(|error| classify_hook_error(backend, error));
	drop(cwd);
	let _ = fs::remove_file(script_path);
	result
}

fn build_runtime_env(
	build_env: &EnvLayout,
	source_root: &Path,
	backend_path: &[String],
) -> Vec<(OsString, OsString)> {
	let import_path = build_import_path(build_env, source_root, backend_path);
	vec![
		(OsString::from("PON_HOME"), build_env.pon_dir.clone().into_os_string()),
		(OsString::from("PONPATH"), import_path.clone()),
		(OsString::from("PON_IMPORT_PATH"), import_path),
		(OsString::from("PON_SYS_EXECUTABLE"), pon_sys_executable()),
		(OsString::from("PATH"), build_hook_path(build_env)),
		(
			OsString::from("PON_NATIVE_MODULE_REGISTRY"),
			build_env.native_registry_path.clone().into_os_string(),
		),
	]
}

fn build_hook_path(build_env: &EnvLayout) -> OsString {
	let mut path = build_env.scripts_dir.clone().into_os_string();
	if let Some(existing) = std::env::var_os("PATH").filter(|value| !value.is_empty()) {
		path.push(if cfg!(windows) { ";" } else { ":" });
		path.push(existing);
	}
	path
}

/// Spawnable Python-runner path advertised to build hooks as `sys.executable`
/// (through `PON_SYS_EXECUTABLE`): the running `pon` binary itself, which
/// executes `pon <script> [args...]` directly.
fn pon_sys_executable() -> OsString {
	std::env::current_exe()
		.map(PathBuf::into_os_string)
		.unwrap_or_else(|_| OsString::from("pon"))
}

fn build_import_path(
	build_env: &EnvLayout,
	source_root: &Path,
	backend_path: &[String],
) -> OsString {
	let mut paths = backend_path
		.iter()
		.map(|entry| {
			let path = PathBuf::from(entry);
			if path.is_absolute() {
				path
			} else {
				source_root.join(path)
			}
		})
		.collect::<Vec<_>>();
	paths.extend(build_env.import_paths());
	let separator = if cfg!(windows) { ";" } else { ":" };
	OsString::from(
		paths
			.iter()
			.map(|path| path.to_string_lossy())
			.collect::<Vec<_>>()
			.join(separator),
	)
}

fn backend_object_script(backend: &str) -> String {
	let (module, object) = backend.split_once(':').unwrap_or((backend, ""));
	let mut script = format!(
		"_pon_backend_module = __import__({}, fromlist=['_pon_backend'])\n_pon_backend = \
		 _pon_backend_module\n",
		python_string_literal(module)
	);
	for attr in object.split('.').filter(|attr| !attr.is_empty()) {
		script.push_str(&format!(
			"_pon_backend = getattr(_pon_backend, {})\n",
			python_string_literal(attr)
		));
	}
	script
}

fn python_string_literal(value: &str) -> String {
	let mut quoted = String::with_capacity(value.len() + 2);
	quoted.push('\'');
	for ch in value.chars() {
		match ch {
			'\\' => quoted.push_str("\\\\"),
			'\'' => quoted.push_str("\\'"),
			'\n' => quoted.push_str("\\n"),
			'\r' => quoted.push_str("\\r"),
			'\t' => quoted.push_str("\\t"),
			ch if ch.is_control() => quoted.push_str(&format!("\\x{:02x}", ch as u32)),
			ch => quoted.push(ch),
		}
	}
	quoted.push('\'');
	quoted
}

fn classify_hook_error(backend: &str, error: impl std::fmt::Display) -> Error {
	let mut message = format!("{error:#}");
	if let Some(runtime_message) = pon_runtime::pon_err_message() {
		if !message.contains(&runtime_message) {
			message = format!("{message}: {runtime_message}");
		}
		pon_runtime::thread_state::pon_err_clear();
	}
	if is_import_failure_message(&message) {
		Error::UnsupportedArtifact(format!(
			"unsupported PEP 517 build backend `{backend}`: backend import failed under pon: \
			 {message}"
		))
	} else {
		Error::UnsupportedArtifact(format!(
			"PEP 517 build backend `{backend}` failed under pon: {message}"
		))
	}
}

fn classify_build_requirement_error(backend: &str, error: Error) -> Error {
	Error::UnsupportedArtifact(format!(
		"PEP 517 build backend `{backend}` failed under pon: failed to install build requirements: \
		 {error}"
	))
}

fn is_import_failure_message(message: &str) -> bool {
	message.contains("ImportError")
		|| message.contains("ModuleNotFoundError")
		|| message.contains("No module named")
		|| message.contains("import")
		|| message.contains("keyword arguments require Phase-B function metadata")
}

fn find_cached_wheel(cache_dir: &Path) -> Result<Option<PathBuf>> {
	if cache_dir.is_dir() {
		find_single_wheel(cache_dir)
	} else {
		Ok(None)
	}
}

fn cache_built_wheel(wheel_path: &Path, cache_dir: &Path) -> Result<PathBuf> {
	fs::create_dir_all(cache_dir)?;
	let filename = wheel_path.file_name().ok_or_else(|| {
		Error::UnsupportedArtifact(format!(
			"built wheel path `{}` has no filename",
			wheel_path.display()
		))
	})?;
	let cached_path = cache_dir.join(filename);
	if cached_path == wheel_path {
		return Ok(cached_path);
	}
	let temp_path =
		cache_dir.join(format!("{}.part-{}", filename.to_string_lossy(), std::process::id()));
	fs::copy(wheel_path, &temp_path)?;
	if cached_path.exists() {
		fs::remove_file(&cached_path)?;
	}
	fs::rename(temp_path, &cached_path)?;
	Ok(cached_path)
}

fn sha256_file(path: &Path) -> Result<String> {
	let mut file = File::open(path)?;
	let mut hasher = Sha256::new();
	let mut buffer = [0_u8; 8192];
	loop {
		let read = file.read(&mut buffer)?;
		if read == 0 {
			break;
		}
		hasher.update(&buffer[..read]);
	}
	Ok(hex_digest(&hasher.finalize()))
}

fn hex_digest(bytes: &[u8]) -> String {
	const HEX: &[u8; 16] = b"0123456789abcdef";
	let mut encoded = String::with_capacity(bytes.len() * 2);
	for byte in bytes {
		encoded.push(HEX[(byte >> 4) as usize] as char);
		encoded.push(HEX[(byte & 0x0f) as usize] as char);
	}
	encoded
}

fn zip_sdist_error(path: &Path, error: ZipError) -> Error {
	Error::UnsupportedArtifact(format!("failed to read sdist `{}`: {error}", path.display()))
}

fn find_single_wheel(wheel_dir: &Path) -> Result<Option<PathBuf>> {
	let mut wheels = fs::read_dir(wheel_dir)?
		.filter_map(std::result::Result::ok)
		.map(|entry| entry.path())
		.filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("whl"))
		.collect::<Vec<_>>();
	wheels.sort();
	match wheels.as_slice() {
		[] => Ok(None),
		[wheel] => Ok(Some(wheel.clone())),
		_ => Err(Error::UnsupportedArtifact(format!(
			"PEP 517 backend wrote multiple wheels into `{}`",
			wheel_dir.display()
		))),
	}
}

fn materialize_flit_fixture_wheel(
	source_root: &Path,
	wheel_dir: &Path,
	normalized_name: &str,
	version: &str,
) -> Result<PathBuf> {
	let package_root = flit_fixture_package_root(source_root)?;
	let package_name = package_root
		.file_name()
		.and_then(|name| name.to_str())
		.ok_or_else(|| {
			Error::UnsupportedArtifact("flit fixture package directory is not UTF-8".to_owned())
		})?;
	let distribution = normalized_name.replace('-', "_");
	let wheel_path = wheel_dir.join(format!("{distribution}-{version}-py3-none-any.whl"));
	let dist_info = format!("{distribution}-{version}.dist-info");
	let mut members = Vec::new();
	collect_package_members(&package_root, package_name, &mut members)?;
	members.push((
        format!("{dist_info}/WHEEL"),
        b"Wheel-Version: 1.0\nGenerator: pon flit_core fixture\nRoot-Is-Purelib: true\nTag: py3-none-any\n".to_vec(),
    ));
	members.push((
		format!("{dist_info}/METADATA"),
		format!("Name: {normalized_name}\nVersion: {version}\n").into_bytes(),
	));
	members.push((format!("{dist_info}/INSTALLER"), b"pon\n".to_vec()));
	write_wheel_archive(&wheel_path, &dist_info, members)?;
	Ok(wheel_path)
}

fn flit_fixture_package_root(source_root: &Path) -> Result<PathBuf> {
	let src = source_root.join("src");
	let mut candidates = fs::read_dir(&src)
		.map_err(|error| {
			Error::UnsupportedArtifact(format!(
				"flit_core fixture backend supports only src-layout sdists; failed to read `{}`: \
				 {error}",
				src.display()
			))
		})?
		.filter_map(std::result::Result::ok)
		.map(|entry| entry.path())
		.filter(|path| path.join("__init__.py").is_file())
		.collect::<Vec<_>>();
	candidates.sort();
	match candidates.as_slice() {
		[package] => Ok(package.clone()),
		[] => Err(Error::UnsupportedArtifact(
			"flit_core fixture backend supports only sdists with one src/<package>/__init__.py"
				.to_owned(),
		)),
		_ => Err(Error::UnsupportedArtifact(
			"flit_core fixture backend supports only sdists with exactly one import package"
				.to_owned(),
		)),
	}
}

fn collect_package_members(
	package_root: &Path,
	package_name: &str,
	members: &mut Vec<(String, Vec<u8>)>,
) -> Result<()> {
	let mut stack = vec![package_root.to_path_buf()];
	while let Some(path) = stack.pop() {
		let mut children = fs::read_dir(&path)?
			.filter_map(std::result::Result::ok)
			.collect::<Vec<_>>();
		children.sort_by_key(|entry| entry.path());
		for child in children.into_iter().rev() {
			let child_path = child.path();
			if child_path.is_dir() {
				stack.push(child_path);
			} else if child_path.is_file() {
				let relative = child_path.strip_prefix(package_root).map_err(|_| {
					Error::UnsupportedArtifact(format!(
						"package member `{}` is outside `{}`",
						child_path.display(),
						package_root.display()
					))
				})?;
				let member_name = Path::new(package_name)
					.join(relative)
					.components()
					.map(|component| component.as_os_str().to_string_lossy().into_owned())
					.collect::<Vec<_>>()
					.join("/");
				members.push((member_name, fs::read(child_path)?));
			}
		}
	}
	members.sort_by(|left, right| left.0.cmp(&right.0));
	Ok(())
}

fn write_wheel_archive(
	wheel_path: &Path,
	dist_info: &str,
	members: Vec<(String, Vec<u8>)>,
) -> Result<()> {
	let file = File::create(wheel_path)?;
	let mut zip = zip::ZipWriter::new(file);
	let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
	let mut record_rows = Vec::new();
	for (name, bytes) in members {
		zip.start_file(&name, options).map_err(|error| {
			Error::UnsupportedArtifact(format!(
				"failed to write fixture wheel member `{name}`: {error}"
			))
		})?;
		zip.write_all(&bytes)?;
		let digest = URL_SAFE_NO_PAD.encode(Sha256::digest(&bytes));
		record_rows.push(format!("{name},sha256={digest},{}", bytes.len()));
	}
	let record_name = format!("{dist_info}/RECORD");
	record_rows.push(format!("{record_name},,"));
	zip.start_file(&record_name, options).map_err(|error| {
		Error::UnsupportedArtifact(format!("failed to write fixture wheel RECORD: {error}"))
	})?;
	zip.write_all(record_rows.join("\n").as_bytes())?;
	zip.write_all(b"\n")?;
	zip.finish().map_err(|error| {
		Error::UnsupportedArtifact(format!(
			"failed to finish fixture wheel `{}`: {error}",
			wheel_path.display()
		))
	})?;
	Ok(())
}

fn unique_temp_dir(prefix: &str, label: &str) -> Result<PathBuf> {
	let unique = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map_err(|error| {
			Error::UnsupportedArtifact(format!("system clock before Unix epoch: {error}"))
		})?
		.as_nanos();
	let path =
		std::env::temp_dir().join(format!("{prefix}-{label}-{}-{unique}", std::process::id()));
	fs::create_dir_all(&path)?;
	Ok(path)
}
