use std::{
	collections::BTreeMap,
	fs,
	io::Read,
	path::{Path, PathBuf},
	str::FromStr,
};

use pep440_rs::{Version, VersionSpecifiers};
use sha2::{Digest, Sha256};

use crate::{
	error::{Error, Result},
	names,
	resolve::source::PackageKind,
	wheel::{
		compat::{any_supported, default_supported_tags},
		filename::WheelFilename,
	},
};

pub mod download;
mod html;
mod simple_json;

pub use simple_json::{MultiIndex, SimpleJsonIndex, parse_project_json};

pub const DEFAULT_INDEX_URL: &str = "https://pypi.org/simple/";
pub const NO_OB_REFCNT_C_ABI_REFUSAL: &str =
	"refusing numpy: no-ob_refcnt C-ABI support is available in Pon";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectPage {
	pub meta_api_version: String,
	pub name:             String,
	pub files:            Vec<ProjectFile>,
}

impl ProjectPage {
	#[must_use]
	pub fn versions(&self) -> Vec<Version> {
		let mut versions = self
			.files
			.iter()
			.map(|file| file.version.clone())
			.collect::<Vec<_>>();
		versions.sort();
		versions.dedup();
		versions
	}

	#[must_use]
	pub fn best_match(&self, specifiers: &VersionSpecifiers) -> Option<ProjectFile> {
		self
			.files
			.iter()
			.filter(|file| !file.requires_python_invalid)
			.filter(|file| specifiers.contains(&file.version))
			.max_by(|left, right| {
				left
					.version
					.cmp(&right.version)
					.then_with(|| yanked_rank(left).cmp(&yanked_rank(right)))
					.then_with(|| package_kind_rank(left).cmp(&package_kind_rank(right)))
					.then_with(|| left.filename.cmp(&right.filename))
			})
			.cloned()
	}
}

fn yanked_rank(file: &ProjectFile) -> u8 {
	u8::from(file.yanked.is_none())
}

fn package_kind_rank(file: &ProjectFile) -> u8 {
	match file.kind {
		PackageKind::Pure => 2,
		PackageKind::Native => 1,
		PackageKind::CAbiRefused { .. } => 0,
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectFile {
	pub filename:                String,
	pub url:                     String,
	pub version:                 Version,
	pub kind:                    PackageKind,
	pub hashes:                  BTreeMap<String, String>,
	pub requires_python:         Option<VersionSpecifiers>,
	pub requires_python_invalid: bool,
	pub yanked:                  Option<String>,
	pub dist_info_metadata:      Option<DistInfoMetadata>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DistInfoMetadata {
	pub hashes: BTreeMap<String, String>,
}

pub trait PackageIndex {
	fn lookup(&self, name: &str) -> Result<Option<ProjectPage>>;

	fn distribution_metadata(&self, _file: &ProjectFile) -> Result<Option<String>> {
		Ok(None)
	}

	fn fetch_artifact(&self, file: &ProjectFile) -> Result<PathBuf>;
}

impl<T: PackageIndex + ?Sized> PackageIndex for &T {
	fn lookup(&self, name: &str) -> Result<Option<ProjectPage>> {
		(**self).lookup(name)
	}

	fn distribution_metadata(&self, file: &ProjectFile) -> Result<Option<String>> {
		(**self).distribution_metadata(file)
	}

	fn fetch_artifact(&self, file: &ProjectFile) -> Result<PathBuf> {
		(**self).fetch_artifact(file)
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SelectedIndex {
	Catalog(CatalogIndex),
	SimpleJson(MultiIndex),
	Local(LocalIndex),
}

impl SelectedIndex {
	#[must_use]
	pub fn catalog() -> Self {
		Self::Catalog(CatalogIndex::new())
	}

	#[must_use]
	pub fn simple_json<I, U>(
		index_urls: I,
		pon_home: impl Into<PathBuf>,
		allow_unhashed: bool,
	) -> Self
	where
		I: IntoIterator<Item = U>,
		U: Into<String>,
	{
		let cache_dir = pon_home.into().join("cache/http");
		let indexes = index_urls
			.into_iter()
			.map(|url| {
				SimpleJsonIndex::with_cache_dir_and_hash_policy(
					url,
					cache_dir.clone(),
					allow_unhashed,
				)
			})
			.collect();
		Self::SimpleJson(MultiIndex::new(indexes))
	}

	#[must_use]
	pub fn local(cache_root: impl Into<PathBuf>, find_links: Vec<PathBuf>) -> Self {
		Self::Local(LocalIndex::new(cache_root, find_links))
	}
}

impl PackageIndex for SelectedIndex {
	fn lookup(&self, name: &str) -> Result<Option<ProjectPage>> {
		match self {
			Self::Catalog(index) => index.lookup(name),
			Self::SimpleJson(index) => index.lookup(name),
			Self::Local(index) => index.lookup(name),
		}
	}

	fn distribution_metadata(&self, file: &ProjectFile) -> Result<Option<String>> {
		match self {
			Self::Catalog(index) => index.distribution_metadata(file),
			Self::SimpleJson(index) => index.distribution_metadata(file),
			Self::Local(index) => index.distribution_metadata(file),
		}
	}

	fn fetch_artifact(&self, file: &ProjectFile) -> Result<PathBuf> {
		match self {
			Self::Catalog(index) => index.fetch_artifact(file),
			Self::SimpleJson(index) => index.fetch_artifact(file),
			Self::Local(index) => index.fetch_artifact(file),
		}
	}
}

/// Local-only package index backed by the artifact cache and explicit find-links roots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalIndex {
	roots: Vec<PathBuf>,
}

impl LocalIndex {
	#[must_use]
	pub fn new(cache_root: impl Into<PathBuf>, find_links: Vec<PathBuf>) -> Self {
		let mut roots = vec![cache_root.into()];
		roots.extend(find_links);
		Self { roots }
	}
}

impl PackageIndex for LocalIndex {
	fn lookup(&self, name: &str) -> Result<Option<ProjectPage>> {
		let normalized = normalized_project_name(name)?;
		let mut files = Vec::new();
		for root in &self.roots {
			for path in local_artifact_paths(root)? {
				let Some(file) = local_project_file(&path)? else {
					continue;
				};
				if project_name_from_filename(&file.filename).as_deref() == Some(normalized.as_str()) {
					files.push(file);
				}
			}
		}
		files.sort_by(|left, right| {
			left
				.version
				.cmp(&right.version)
				.then_with(|| left.filename.cmp(&right.filename))
				.then_with(|| left.url.cmp(&right.url))
		});
		files.dedup_by(|left, right| left.filename == right.filename && left.url == right.url);
		Ok((!files.is_empty()).then_some(ProjectPage {
			meta_api_version: "local".to_owned(),
			name: normalized,
			files,
		}))
	}

	fn fetch_artifact(&self, file: &ProjectFile) -> Result<PathBuf> {
		if let Some(path) = file_url_path(&file.url).filter(|path| path.is_file()) {
			return Ok(path);
		}
		let path = Path::new(&file.filename);
		if path.is_file() {
			return Ok(path.to_path_buf());
		}
		Err(Error::UnsupportedArtifact(format!(
			"artifact `{}` is not available in the local cache or find-links roots",
			file.filename
		)))
	}
}


#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CatalogIndex;

impl CatalogIndex {
	#[must_use]
	pub fn new() -> Self {
		Self
	}

	pub fn versions(&self, name: impl AsRef<str>) -> Result<Vec<Version>> {
		Ok(self
			.lookup(name.as_ref())?
			.map_or_else(Vec::new, |project| project.versions()))
	}
}

impl PackageIndex for CatalogIndex {
	fn lookup(&self, name: &str) -> Result<Option<ProjectPage>> {
		let normalized = normalized_project_name(name)?;
		Ok(match normalized.as_str() {
			"idna" => Some(project("idna", [
				file("idna-3.9-py3-none-any.whl", "3.9", PackageKind::Pure)?,
				file("idna-3.10-py3-none-any.whl", "3.10", PackageKind::Pure)?,
			])),
			"flit-core" => Some(project("flit-core", [file(
				"flit_core-3.12.0-py3-none-any.whl",
				"3.12.0",
				PackageKind::Pure,
			)?])),
			"numpy" => Some(project("numpy", [file(
				"numpy-2.3.1-cp314-cp314-macosx_14_0_arm64.whl",
				"2.3.1",
				PackageKind::CAbiRefused { reason: NO_OB_REFCNT_C_ABI_REFUSAL.to_owned() },
			)?])),
			_ => None,
		})
	}

	fn fetch_artifact(&self, file: &ProjectFile) -> Result<PathBuf> {
		let filename_path = Path::new(&file.filename);
		if filename_path.is_file() {
			return Ok(filename_path.to_path_buf());
		}

		if let Some(path) = file_url_path(&file.url).filter(|path| path.is_file()) {
			return Ok(path);
		}

		let basename = filename_path
			.file_name()
			.and_then(|name| name.to_str())
			.unwrap_or(file.filename.as_str());
		for fixture_kind in ["wheels", "sdists"] {
			let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
				.join("fixtures")
				.join(fixture_kind)
				.join(basename);
			if fixture_path.is_file() {
				return Ok(fixture_path);
			}
		}

		Err(Error::UnsupportedArtifact(format!(
			"artifact `{}` is not available in the bundled Pon fixtures",
			file.filename
		)))
	}
}

fn file_url_path(url: &str) -> Option<PathBuf> {
	let path = url.strip_prefix("file://")?;
	if let Some(path) = path.strip_prefix("localhost/") {
		Some(PathBuf::from(format!("/{path}")))
	} else {
		Some(PathBuf::from(path))
	}
}

fn normalized_project_name(name: &str) -> Result<String> {
	names::validate(name)?;
	Ok(names::normalize(name))
}

fn project<const N: usize>(name: &str, files: [ProjectFile; N]) -> ProjectPage {
	ProjectPage {
		meta_api_version: "1.0".to_owned(),
		name:             name.to_owned(),
		files:            Vec::from(files),
	}
}

fn file(filename: &str, version: &str, kind: PackageKind) -> Result<ProjectFile> {
	let fixture_path = fixture_artifact_path(filename);
	let (url, hashes) = if let Some(path) = fixture_path.as_ref() {
		let mut hashes = BTreeMap::new();
		hashes.insert("sha256".to_owned(), sha256_file(path)?);
		(format!("file://{}", path.display()), hashes)
	} else {
		(format!("{DEFAULT_INDEX_URL}{filename}"), BTreeMap::new())
	};
	Ok(ProjectFile {
		filename: filename.to_owned(),
		url,
		version: Version::from_str(version)
			.map_err(|_| Error::InvalidRequirement(filename.to_owned()))?,
		kind,
		hashes,
		requires_python: None,
		requires_python_invalid: false,
		yanked: None,
		dist_info_metadata: None,
	})
}

fn fixture_artifact_path(filename: &str) -> Option<PathBuf> {
	for fixture_kind in ["wheels", "sdists"] {
		let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
			.join("fixtures")
			.join(fixture_kind)
			.join(filename);
		if path.is_file() {
			return Some(path);
		}
	}
	None
}

fn local_artifact_paths(root: &Path) -> Result<Vec<PathBuf>> {
	if !root.exists() {
		return Ok(Vec::new());
	}
	let mut pending = vec![root.to_path_buf()];
	let mut paths = Vec::new();
	while let Some(path) = pending.pop() {
		let metadata = match fs::metadata(&path) {
			Ok(metadata) => metadata,
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
			Err(error) => return Err(error.into()),
		};
		if metadata.is_dir() {
			for entry in fs::read_dir(&path)? {
				pending.push(entry?.path());
			}
		} else if metadata.is_file() && supported_artifact_filename(&path) {
			paths.push(path);
		}
	}
	Ok(paths)
}

fn supported_artifact_filename(path: &Path) -> bool {
	path
		.file_name()
		.and_then(|name| name.to_str())
		.is_some_and(|name| name.ends_with(".whl") || name.ends_with(".tar.gz") || name.ends_with(".zip"))
}

fn local_project_file(path: &Path) -> Result<Option<ProjectFile>> {
	let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
		return Ok(None);
	};
	let Some(version) = version_from_filename(filename) else {
		return Ok(None);
	};
	let Some(kind) = kind_from_filename(filename) else {
		return Ok(None);
	};
	let mut hashes = BTreeMap::new();
	hashes.insert("sha256".to_owned(), sha256_file(path)?);
	Ok(Some(ProjectFile {
		filename: filename.to_owned(),
		url: format!("file://{}", path.display()),
		version,
		kind,
		hashes,
		requires_python: None,
		requires_python_invalid: false,
		yanked: None,
		dist_info_metadata: None,
	}))
}

fn project_name_from_filename(filename: &str) -> Option<String> {
	if let Ok(wheel) = WheelFilename::parse(filename) {
		return Some(wheel.normalized_distribution);
	}
	let stem = filename
		.strip_suffix(".tar.gz")
		.or_else(|| filename.strip_suffix(".zip"))?;
	let (name, version) = stem.rsplit_once('-')?;
	Version::from_str(version).ok()?;
	Some(names::normalize(name))
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

fn kind_from_filename(filename: &str) -> Option<PackageKind> {
	let Ok(wheel) = WheelFilename::parse(filename) else {
		return Some(PackageKind::Pure);
	};
	if any_supported(&wheel.tags(), &default_supported_tags()) {
		return Some(PackageKind::Pure);
	}
	if wheel.abi_tag.split('.').any(is_refcount_cpython_abi) {
		return Some(PackageKind::CAbiRefused {
			reason: NO_OB_REFCNT_C_ABI_REFUSAL.to_owned(),
		});
	}
	Some(PackageKind::Native)
}

fn is_refcount_cpython_abi(tag: &str) -> bool {
	tag.starts_with("cp") && tag != "abi3" && tag != "none"
}

fn sha256_file(path: &Path) -> Result<String> {
	let mut file = fs::File::open(path)?;
	let mut hasher = Sha256::new();
	let mut buffer = [0_u8; 64 * 1024];
	loop {
		let read = file.read(&mut buffer)?;
		if read == 0 {
			break;
		}
		hasher.update(&buffer[..read]);
	}
	Ok(hex_bytes(&hasher.finalize()))
}

fn hex_bytes(bytes: &[u8]) -> String {
	const HEX: &[u8; 16] = b"0123456789abcdef";
	let mut output = String::with_capacity(bytes.len() * 2);
	for byte in bytes {
		output.push(HEX[(byte >> 4) as usize] as char);
		output.push(HEX[(byte & 0x0f) as usize] as char);
	}
	output
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn package_index_lookup_normalizes_names_and_returns_pep_691_shape() {
		let index = CatalogIndex::new();
		let project = PackageIndex::lookup(&index, "Flit_Core")
			.expect("lookup")
			.expect("project");

		assert_eq!(project.meta_api_version, "1.0");
		assert_eq!(project.name, "flit-core");
		assert_eq!(project.files[0].filename, "flit_core-3.12.0-py3-none-any.whl");
		assert_eq!(
			project.files[0].hashes.get("sha256").map(String::as_str),
			Some("c3c2f513473d9910010bca2c91dcc8a3f13f73153a600119830af6ebff01d4df")
		);
		assert!(project.files[0].requires_python.is_none());
		assert!(!project.files[0].requires_python_invalid);
		assert_eq!(project.files[0].yanked, None);
	}

	#[test]
	fn version_set_selects_highest_matching_catalog_file() {
		let index = CatalogIndex::new();
		let project = index.lookup("idna").expect("lookup").expect("project");
		let version_set = VersionSpecifiers::from_str("<3.10").expect("version set");
		let best = project.best_match(&version_set).expect("best match");

		assert_eq!(
			project
				.versions()
				.iter()
				.map(ToString::to_string)
				.collect::<Vec<_>>(),
			["3.9", "3.10"]
		);
		assert_eq!(best.version.to_string(), "3.9");
	}

	#[test]
	fn version_set_prefers_installable_non_yanked_file_for_same_version() {
		let pure = file("demo-1.0.0-py3-none-any.whl", "1.0.0", PackageKind::Pure).expect("pure");
		let mut yanked =
			file("demo-1.0.0-py3-none-any.whl", "1.0.0", PackageKind::Pure).expect("yanked");
		yanked.filename = "demo-1.0.0-yanked-py3-none-any.whl".to_owned();
		yanked.yanked = Some("bad file".to_owned());
		let native = file("demo-1.0.0.tar.gz", "1.0.0", PackageKind::Native).expect("sdist");
		let project = ProjectPage {
			meta_api_version: "1.0".to_owned(),
			name:             "demo".to_owned(),
			files:            vec![native, yanked, pure.clone()],
		};

		let best = project
			.best_match(&VersionSpecifiers::default())
			.expect("best");

		assert_eq!(best, pure);
	}

	#[test]
	fn numpy_entry_carries_no_ob_refcnt_refusal_metadata() {
		let index = CatalogIndex::new();
		let project = index.lookup("numpy").expect("lookup").expect("project");

		assert_eq!(project.files[0].kind, PackageKind::CAbiRefused {
			reason: NO_OB_REFCNT_C_ABI_REFUSAL.to_owned(),
		});
	}

	#[test]
	fn local_index_reads_find_links_artifacts_with_hashes() {
		let temp = std::env::temp_dir().join(format!(
			"pon-local-index-{}-{}",
			std::process::id(),
			std::time::SystemTime::now()
				.duration_since(std::time::UNIX_EPOCH)
				.expect("clock")
				.as_nanos()
		));
		let wheelhouse = temp.join("wheelhouse");
		fs::create_dir_all(&wheelhouse).expect("wheelhouse");
		let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
			.join("fixtures")
			.join("wheels")
			.join("idna-3.10-py3-none-any.whl");
		let copied = wheelhouse.join("idna-3.10-py3-none-any.whl");
		fs::copy(&fixture, &copied).expect("copy fixture wheel");
		let index = LocalIndex::new(temp.join("cache"), vec![wheelhouse]);

		let project = index.lookup("IDNA").expect("lookup").expect("project");

		assert_eq!(project.name, "idna");
		assert_eq!(project.files.len(), 1);
		assert_eq!(project.files[0].filename, "idna-3.10-py3-none-any.whl");
		let copied_hash = sha256_file(&copied).expect("hash");
		assert_eq!(
			project.files[0].hashes.get("sha256").map(String::as_str),
			Some(copied_hash.as_str())
		);
		let _ = fs::remove_dir_all(temp);
	}
}
