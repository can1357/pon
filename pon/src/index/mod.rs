use std::{
	collections::BTreeMap,
	path::{Path, PathBuf},
	str::FromStr,
};

use pep440_rs::{Version, VersionSpecifiers};

use crate::{
	error::{Error, Result},
	names,
	resolve::source::PackageKind,
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
}

impl SelectedIndex {
	#[must_use]
	pub fn catalog() -> Self {
		Self::Catalog(CatalogIndex::new())
	}

	#[must_use]
	pub fn simple_json<I, U>(index_urls: I, pon_home: impl Into<PathBuf>) -> Self
	where
		I: IntoIterator<Item = U>,
		U: Into<String>,
	{
		let cache_dir = pon_home.into().join("cache/http");
		let indexes = index_urls
			.into_iter()
			.map(|url| SimpleJsonIndex::with_cache_dir(url, cache_dir.clone()))
			.collect();
		Self::SimpleJson(MultiIndex::new(indexes))
	}
}

impl PackageIndex for SelectedIndex {
	fn lookup(&self, name: &str) -> Result<Option<ProjectPage>> {
		match self {
			Self::Catalog(index) => index.lookup(name),
			Self::SimpleJson(index) => index.lookup(name),
		}
	}

	fn distribution_metadata(&self, file: &ProjectFile) -> Result<Option<String>> {
		match self {
			Self::Catalog(index) => index.distribution_metadata(file),
			Self::SimpleJson(index) => index.distribution_metadata(file),
		}
	}

	fn fetch_artifact(&self, file: &ProjectFile) -> Result<PathBuf> {
		match self {
			Self::Catalog(index) => index.fetch_artifact(file),
			Self::SimpleJson(index) => index.fetch_artifact(file),
		}
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
	Ok(ProjectFile {
		filename: filename.to_owned(),
		url: format!("{DEFAULT_INDEX_URL}{filename}"),
		version: Version::from_str(version)
			.map_err(|_| Error::InvalidRequirement(filename.to_owned()))?,
		kind,
		hashes: BTreeMap::new(),
		requires_python: None,
		requires_python_invalid: false,
		yanked: None,
		dist_info_metadata: None,
	})
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
		assert!(project.files[0].hashes.is_empty());
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
}
