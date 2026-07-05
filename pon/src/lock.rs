use std::{
	collections::BTreeMap,
	fmt, fs,
	io::ErrorKind,
	path::{Path, PathBuf},
	str::FromStr,
};

use sha2::{Digest, Sha256};
use toml_edit::{ArrayOfTables, DocumentMut, InlineTable, Item, Table, TableLike, Value, value};

use crate::{
	error::{Error, Result},
	index::{
		ProjectFile,
		download::{HashPolicy, download_artifact},
	},
	install::{PackageArtifact, ResolvedRecord, direct_url::DirectUrl},
	pyproject::PyProject,
	resolve::{
		Resolution, ResolvedArtifact, ResolvedDist,
		source::{PackageKind, PackageRecord},
	},
	vcs,
};

pub const LOCK_VERSION: &str = "1.0";
pub const CREATED_BY: &str = concat!("pon ", env!("CARGO_PKG_VERSION"));
pub const DEFAULT_REQUIRES_PYTHON: &str = ">=3.14";
pub const LOCK_FILE_NAME: &str = "pon.lock";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockFile {
	pub lock_version:    String,
	pub created_by:      String,
	pub requires_python: String,
	pub input_hash:      String,
	pub packages:        Vec<LockedPackage>,
}

impl LockFile {
	#[must_use]
	pub fn new(packages: Vec<LockedPackage>) -> Self {
		Self::with_input_hash(
			packages,
			DEFAULT_REQUIRES_PYTHON,
			compute_input_hash(std::iter::empty::<&str>(), None, false),
		)
	}

	#[must_use]
	pub fn with_input_hash(
		mut packages: Vec<LockedPackage>,
		requires_python: impl Into<String>,
		input_hash: impl Into<String>,
	) -> Self {
		packages.sort_by(|left, right| {
			left
				.name
				.cmp(&right.name)
				.then_with(|| left.version.cmp(&right.version))
		});
		Self {
			lock_version: LOCK_VERSION.to_owned(),
			created_by: CREATED_BY.to_owned(),
			requires_python: requires_python.into(),
			input_hash: input_hash.into(),
			packages,
		}
	}

	#[must_use]
	pub fn from_records(records: &[PackageRecord]) -> Self {
		let dependencies = records
			.iter()
			.map(|record| format!("{}=={}", record.name, record.version))
			.collect::<Vec<_>>();
		Self::with_input_hash(
			records.iter().map(LockedPackage::from).collect(),
			DEFAULT_REQUIRES_PYTHON,
			compute_input_hash(dependencies.iter().map(String::as_str), None, false),
		)
	}

	#[must_use]
	pub fn from_resolution(
		resolution: &Resolution,
		requires_python: impl Into<String>,
		input_hash: impl Into<String>,
	) -> Self {
		Self::with_input_hash(
			resolution
				.dists
				.iter()
				.map(LockedPackage::from_resolved_dist)
				.collect(),
			requires_python,
			input_hash,
		)
	}

	#[must_use]
	pub fn from_resolution_for_pyproject(resolution: &Resolution, pyproject: &PyProject) -> Self {
		Self::from_resolution(
			resolution,
			DEFAULT_REQUIRES_PYTHON,
			compute_project_input_hash(pyproject),
		)
	}

	#[must_use]
	pub fn from_resolution_with_project_options(
		resolution: &Resolution,
		pyproject: &PyProject,
		index_url: Option<&str>,
		allow_prerelease: Option<bool>,
	) -> Self {
		Self::from_resolution(
			resolution,
			DEFAULT_REQUIRES_PYTHON,
			compute_project_input_hash_with_overrides(pyproject, index_url, allow_prerelease),
		)
	}

	#[must_use]
	pub fn is_stale(&self, expected_input_hash: &str) -> bool {
		self.input_hash != expected_input_hash
	}

	pub fn ensure_fresh(&self, expected_input_hash: &str) -> Result<()> {
		if self.is_stale(expected_input_hash) {
			Err(stale_lock_error())
		} else {
			Ok(())
		}
	}

	pub fn write_to_path(&self, path: impl AsRef<Path>) -> Result<()> {
		fs::write(path, self.to_string())?;
		Ok(())
	}

	pub fn read_from_path(path: impl AsRef<Path>) -> Result<Self> {
		fs::read_to_string(path)?.parse()
	}

	pub fn write_to_project_root(&self, project_root: impl AsRef<Path>) -> Result<()> {
		self.write_to_path(lock_path(project_root))
	}

	pub fn read_from_project_root(project_root: impl AsRef<Path>) -> Result<Option<Self>> {
		match fs::read_to_string(lock_path(project_root)) {
			Ok(contents) => contents.parse().map(Some),
			Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
			Err(error) => Err(error.into()),
		}
	}

	pub fn read_frozen_from_project_root(project_root: impl AsRef<Path>) -> Result<Self> {
		Self::read_from_project_root(project_root)?.ok_or_else(missing_frozen_lock_error)
	}

	pub fn read_for_install(
		project_root: impl AsRef<Path>,
		expected_input_hash: &str,
		frozen: bool,
	) -> Result<Option<Self>> {
		let Some(lock) = Self::read_from_project_root(project_root)? else {
			return if frozen {
				Err(missing_frozen_lock_error())
			} else {
				Ok(None)
			};
		};

		if lock.is_stale(expected_input_hash) {
			return if frozen {
				Err(stale_lock_error())
			} else {
				Ok(None)
			};
		}

		Ok(Some(lock))
	}

	pub fn read_frozen_for_install(
		project_root: impl AsRef<Path>,
		expected_input_hash: &str,
	) -> Result<Self> {
		Self::read_for_install(project_root, expected_input_hash, true)?
			.ok_or_else(missing_frozen_lock_error)
	}

	pub fn to_resolved_records(
		&self,
		project_root: impl AsRef<Path>,
		cache_dir: impl AsRef<Path>,
	) -> Result<Vec<ResolvedRecord>> {
		let project_root = project_root.as_ref();
		let cache_dir = cache_dir.as_ref();
		self
			.packages
			.iter()
			.map(|package| package.to_resolved_record(project_root, cache_dir))
			.collect()
	}

	fn to_document(&self) -> DocumentMut {
		let mut doc = DocumentMut::new();
		let root = doc.as_table_mut();
		root.insert("lock-version", value(self.lock_version.as_str()));
		root.insert("created-by", value(self.created_by.as_str()));
		root.insert("requires-python", value(self.requires_python.as_str()));

		let mut tool = Table::new();
		tool.set_implicit(true);
		let mut pon = Table::new();
		pon.insert("input-hash", value(self.input_hash.as_str()));
		tool.insert("pon", Item::Table(pon));
		root.insert("tool", Item::Table(tool));

		if !self.packages.is_empty() {
			let mut packages = ArrayOfTables::new();
			for package in &self.packages {
				packages.push(package.to_table());
			}
			root.insert("packages", Item::ArrayOfTables(packages));
		}

		doc
	}
}

impl fmt::Display for LockFile {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(&self.to_document().to_string())
	}
}

impl FromStr for LockFile {
	type Err = Error;

	fn from_str(input: &str) -> Result<Self> {
		parse_lock(input)
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockedPackage {
	pub name:      String,
	pub version:   String,
	pub marker:    Option<String>,
	pub kind:      LockedPackageKind,
	pub wheels:    Vec<LockedWheel>,
	pub sdist:     Option<LockedSdist>,
	pub directory: Option<LockedDirectory>,
	pub vcs:       Option<LockedVcs>,
}

impl LockedPackage {
	#[must_use]
	pub fn pure(name: impl Into<String>, version: impl Into<String>) -> Self {
		Self::with_kind(name, version, LockedPackageKind::Pure)
	}

	#[must_use]
	pub fn native(name: impl Into<String>, version: impl Into<String>) -> Self {
		Self::with_kind(name, version, LockedPackageKind::Native)
	}

	#[must_use]
	pub fn cabi_refused(name: impl Into<String>, version: impl Into<String>) -> Self {
		Self::with_kind(name, version, LockedPackageKind::CAbiRefused)
	}

	#[must_use]
	pub fn with_kind(
		name: impl Into<String>,
		version: impl Into<String>,
		kind: LockedPackageKind,
	) -> Self {
		Self {
			name: name.into(),
			version: version.into(),
			marker: None,
			kind,
			wheels: Vec::new(),
			sdist: None,
			directory: None,
			vcs: None,
		}
	}

	#[must_use]
	pub fn from_resolved_dist(dist: &ResolvedDist) -> Self {
		let mut package = Self::with_kind(
			dist.name.clone(),
			dist.version.to_string(),
			LockedPackageKind::from(&dist.kind),
		);
		package.marker = dist.marker.clone();
		match &dist.artifact {
			ResolvedArtifact::Wheel(file) => package.wheels.push(LockedWheel::from_project_file(file)),
			ResolvedArtifact::Sdist(file) => {
				package.sdist = Some(LockedSdist::from_project_file(file))
			},
			ResolvedArtifact::Dir { path, editable } => {
				package.directory =
					Some(LockedDirectory { path: path.clone(), editable: *editable });
			},
			ResolvedArtifact::Vcs { url, requested_rev, commit, dir: _ } => {
				package.vcs = Some(LockedVcs {
					vcs_type:           "git".to_owned(),
					url:                url.clone(),
					requested_revision: requested_rev.clone(),
					commit_id:          commit.clone(),
				});
			},
		}
		package
	}

	pub fn to_resolved_record(
		&self,
		project_root: &Path,
		cache_dir: &Path,
	) -> Result<ResolvedRecord> {
		if let Some(wheel) = self.wheels.first() {
			return wheel.to_resolved_record(self, cache_dir);
		}
		if let Some(sdist) = &self.sdist {
			return sdist.to_resolved_record(self, cache_dir);
		}
		if let Some(directory) = &self.directory {
			return Ok(ResolvedRecord::dir(
				self.name.clone(),
				self.version.clone(),
				resolve_project_path(project_root, &directory.path),
				directory.editable,
			));
		}
		if let Some(vcs) = &self.vcs {
			return vcs.to_resolved_record(self, cache_dir);
		}

		Err(Error::Cli(format!("pon.lock package `{}` has no installable artifact", self.name)))
	}

	fn to_table(&self) -> Table {
		let mut table = Table::new();
		table.insert("name", value(self.name.as_str()));
		table.insert("version", value(self.version.as_str()));
		if let Some(marker) = &self.marker {
			table.insert("marker", value(marker.as_str()));
		}

		if !self.wheels.is_empty() {
			let mut wheels = ArrayOfTables::new();
			for wheel in &self.wheels {
				wheels.push(wheel.to_table());
			}
			table.insert("wheels", Item::ArrayOfTables(wheels));
		}
		if let Some(sdist) = &self.sdist {
			table.insert("sdist", Item::Table(sdist.to_table()));
		}
		if let Some(directory) = &self.directory {
			table.insert("directory", Item::Table(directory.to_table()));
		}
		if let Some(vcs) = &self.vcs {
			table.insert("vcs", Item::Table(vcs.to_table()));
		}
		if let Some(kind) = self.kind.tool_kind() {
			let mut tool = Table::new();
			tool.set_implicit(true);
			let mut pon = Table::new();
			pon.insert("kind", value(kind));
			tool.insert("pon", Item::Table(pon));
			table.insert("tool", Item::Table(tool));
		}
		table
	}
}

impl From<&PackageRecord> for LockedPackage {
	fn from(record: &PackageRecord) -> Self {
		Self::with_kind(
			record.name.clone(),
			record.version.clone(),
			LockedPackageKind::from(&record.kind),
		)
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockedWheel {
	pub name:   String,
	pub url:    String,
	pub hashes: BTreeMap<String, String>,
}

impl LockedWheel {
	#[must_use]
	pub fn from_project_file(file: &ProjectFile) -> Self {
		Self { name: file.filename.clone(), url: file.url.clone(), hashes: file.hashes.clone() }
	}

	pub fn fetch(&self, cache_dir: &Path) -> Result<PathBuf> {
		fetch_locked_artifact(cache_dir, &self.url, &self.name, &self.hashes)
	}

	#[must_use]
	pub fn direct_url(&self) -> DirectUrl {
		DirectUrl::archive_with_hashes(self.url.clone(), self.hashes.clone())
	}

	pub fn to_resolved_record(
		&self,
		package: &LockedPackage,
		cache_dir: &Path,
	) -> Result<ResolvedRecord> {
		Ok(ResolvedRecord {
			name:     package.name.clone(),
			version:  package.version.clone(),
			artifact: PackageArtifact::Wheel {
				path:       self.fetch(cache_dir)?,
				direct_url: Some(self.direct_url()),
			},
		})
	}

	fn to_table(&self) -> Table {
		file_artifact_table(&self.name, &self.url, &self.hashes)
	}

	fn from_table(table: &dyn TableLike) -> Result<Self> {
		Ok(Self {
			name:   required_string(table, "name", "pon.lock wheel missing name")?,
			url:    required_string(table, "url", "pon.lock wheel missing url")?,
			hashes: parse_hashes(table)?,
		})
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockedSdist {
	pub name:   String,
	pub url:    String,
	pub hashes: BTreeMap<String, String>,
}

impl LockedSdist {
	#[must_use]
	pub fn from_project_file(file: &ProjectFile) -> Self {
		Self { name: file.filename.clone(), url: file.url.clone(), hashes: file.hashes.clone() }
	}

	pub fn fetch(&self, cache_dir: &Path) -> Result<PathBuf> {
		fetch_locked_artifact(cache_dir, &self.url, &self.name, &self.hashes)
	}

	#[must_use]
	pub fn direct_url(&self) -> DirectUrl {
		DirectUrl::archive_with_hashes(self.url.clone(), self.hashes.clone())
	}

	pub fn to_resolved_record(
		&self,
		package: &LockedPackage,
		cache_dir: &Path,
	) -> Result<ResolvedRecord> {
		Ok(ResolvedRecord {
			name:     package.name.clone(),
			version:  package.version.clone(),
			artifact: PackageArtifact::Sdist {
				path:       self.fetch(cache_dir)?,
				direct_url: Some(self.direct_url()),
			},
		})
	}

	fn to_table(&self) -> Table {
		file_artifact_table(&self.name, &self.url, &self.hashes)
	}

	fn from_table(table: &dyn TableLike) -> Result<Self> {
		Ok(Self {
			name:   required_string(table, "name", "pon.lock sdist missing name")?,
			url:    required_string(table, "url", "pon.lock sdist missing url")?,
			hashes: parse_hashes(table)?,
		})
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockedDirectory {
	pub path:     PathBuf,
	pub editable: bool,
}

impl LockedDirectory {
	fn to_table(&self) -> Table {
		let mut table = Table::new();
		table.insert("path", value(self.path.to_string_lossy().as_ref()));
		table.insert("editable", value(self.editable));
		table
	}

	fn from_table(table: &dyn TableLike) -> Result<Self> {
		Ok(Self {
			path:     PathBuf::from(required_string(
				table,
				"path",
				"pon.lock directory missing path",
			)?),
			editable: optional_bool(table, "editable", "pon.lock directory editable")?
				.unwrap_or(false),
		})
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockedVcs {
	pub vcs_type:           String,
	pub url:                String,
	pub requested_revision: Option<String>,
	pub commit_id:          String,
}

impl LockedVcs {
	pub fn to_resolved_record(
		&self,
		package: &LockedPackage,
		cache_dir: &Path,
	) -> Result<ResolvedRecord> {
		if !self.vcs_type.eq_ignore_ascii_case("git") {
			return Err(Error::Cli(format!(
				"unsupported pon.lock VCS type `{}`; only git is supported",
				self.vcs_type
			)));
		}

		let checkout = vcs::fetch_git(cache_dir, &self.url, Some(self.commit_id.as_str()))?;
		Ok(ResolvedRecord::dir(package.name.clone(), package.version.clone(), checkout.dir, false))
	}

	fn to_table(&self) -> Table {
		let mut table = Table::new();
		table.insert("type", value(self.vcs_type.as_str()));
		table.insert("url", value(self.url.as_str()));
		if let Some(revision) = &self.requested_revision {
			table.insert("requested-revision", value(revision.as_str()));
		}
		table.insert("commit-id", value(self.commit_id.as_str()));
		table
	}

	fn from_table(table: &dyn TableLike) -> Result<Self> {
		Ok(Self {
			vcs_type:           required_string(table, "type", "pon.lock VCS entry missing type")?,
			url:                required_string(table, "url", "pon.lock VCS entry missing url")?,
			requested_revision: optional_string(
				table,
				"requested-revision",
				"pon.lock VCS requested-revision",
			)?,
			commit_id:          required_string(
				table,
				"commit-id",
				"pon.lock VCS entry missing commit-id",
			)?,
		})
	}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockedPackageKind {
	Pure,
	Native,
	CAbiRefused,
}

impl LockedPackageKind {
	#[must_use]
	pub fn tool_kind(self) -> Option<&'static str> {
		match self {
			Self::Pure => None,
			Self::Native => Some("native"),
			Self::CAbiRefused => Some("cabi-refused"),
		}
	}
}

impl From<&PackageKind> for LockedPackageKind {
	fn from(kind: &PackageKind) -> Self {
		match kind {
			PackageKind::Pure => Self::Pure,
			PackageKind::Native => Self::Native,
			PackageKind::CAbiRefused { .. } => Self::CAbiRefused,
		}
	}
}

#[must_use]
pub fn lock_path(project_root: impl AsRef<Path>) -> PathBuf {
	project_root.as_ref().join(LOCK_FILE_NAME)
}

#[must_use]
pub fn compute_project_input_hash(pyproject: &PyProject) -> String {
	compute_project_input_hash_with_overrides(pyproject, None, None)
}

#[must_use]
pub fn compute_project_input_hash_with_overrides(
	pyproject: &PyProject,
	index_url: Option<&str>,
	allow_prerelease: Option<bool>,
) -> String {
	let dependencies = pyproject.dependencies();
	let effective_index_url = index_url.or_else(|| pyproject.tool_pon_index_url());
	let effective_allow_prerelease =
		allow_prerelease.unwrap_or_else(|| pyproject.tool_pon_allow_prerelease());

	compute_input_hash(
		dependencies.iter().map(String::as_str),
		effective_index_url,
		effective_allow_prerelease,
	)
}

#[must_use]
pub fn compute_input_hash<I, S>(
	dependencies: I,
	index_url: Option<&str>,
	allow_prerelease: bool,
) -> String
where
	I: IntoIterator<Item = S>,
	S: AsRef<str>,
{
	let mut dependency_lines = dependencies
		.into_iter()
		.map(|dependency| dependency.as_ref().trim().to_owned())
		.filter(|dependency| !dependency.is_empty())
		.collect::<Vec<_>>();
	dependency_lines.sort_unstable();

	let mut input = dependency_lines
		.into_iter()
		.map(|dependency| format!("project.dependencies={dependency}"))
		.collect::<Vec<_>>();
	input.push(format!("tool.pon.index-url={}", index_url.unwrap_or_default()));
	input.push(format!("tool.pon.allow-prerelease={allow_prerelease}"));

	format!("sha256:{}", sha256_hex(input.join("\n").as_bytes()))
}

#[must_use]
pub fn stale_lock_error() -> Error {
	Error::Cli("pon.lock is stale (input-hash mismatch); run `pon lock`".to_owned())
}

#[must_use]
pub fn missing_frozen_lock_error() -> Error {
	Error::Cli("pon.lock not found; run `pon lock`".to_owned())
}

fn fetch_locked_artifact(
	cache_dir: &Path,
	url: &str,
	filename: &str,
	hashes: &BTreeMap<String, String>,
) -> Result<PathBuf> {
	let required_hashes = required_hash_entries(hashes);
	if required_hashes.is_empty() {
		download_artifact(cache_dir, url, filename, &HashPolicy::Index(hashes))
	} else {
		download_artifact(cache_dir, url, filename, &HashPolicy::Required(&required_hashes))
	}
}

fn required_hash_entries(hashes: &BTreeMap<String, String>) -> Vec<String> {
	hashes
		.iter()
		.map(|(algorithm, digest)| format!("{algorithm}:{digest}"))
		.collect()
}

fn resolve_project_path(project_root: &Path, path: &Path) -> PathBuf {
	if path.is_absolute() {
		path.to_path_buf()
	} else {
		project_root.join(path)
	}
}

fn parse_lock(input: &str) -> Result<LockFile> {
	let doc = input
		.parse::<DocumentMut>()
		.map_err(|error| Error::Cli(format!("invalid pon.lock TOML: {error}")))?;
	let root = doc.as_table();

	warn_unknown_top_level_keys(root);

	let lock_version = required_string(root, "lock-version", "pon.lock missing lock-version")?;
	validate_lock_version(&lock_version)?;
	let created_by = required_string(root, "created-by", "pon.lock missing created-by")?;
	let requires_python =
		required_string(root, "requires-python", "pon.lock missing requires-python")?;
	let input_hash = table_at(doc.as_item(), &["tool", "pon"])
		.ok_or_else(|| Error::Cli("pon.lock missing [tool.pon]".to_owned()))
		.and_then(|tool_pon| {
			required_string(tool_pon, "input-hash", "pon.lock missing [tool.pon].input-hash")
		})?;
	let packages = parse_packages(root)?;

	Ok(LockFile { lock_version, created_by, requires_python, input_hash, packages })
}

fn parse_packages(root: &dyn TableLike) -> Result<Vec<LockedPackage>> {
	let Some(item) = root.get("packages") else {
		return Ok(Vec::new());
	};
	let Some(packages) = item.as_array_of_tables() else {
		return Err(Error::Cli("pon.lock packages must be an array of tables".to_owned()));
	};
	packages.iter().map(parse_package).collect()
}

fn parse_package(table: &Table) -> Result<LockedPackage> {
	let name = required_string(table, "name", "pon.lock package missing name")?;
	let version = required_string(table, "version", "pon.lock package missing version")?;
	let marker = optional_string(table, "marker", "pon.lock package marker")?;
	let kind = table_at_table(table, &["tool", "pon"])
		.map(|tool_pon| {
			optional_string(tool_pon, "kind", "pon.lock package kind")?
				.map(|kind| parse_kind(&kind))
				.transpose()
		})
		.transpose()?
		.flatten()
		.unwrap_or(LockedPackageKind::Pure);
	let wheels = parse_wheels(table)?;
	let sdist = parse_optional_table(table, "sdist", "pon.lock package sdist")?
		.map(LockedSdist::from_table)
		.transpose()?;
	let directory = parse_optional_table(table, "directory", "pon.lock package directory")?
		.map(LockedDirectory::from_table)
		.transpose()?;
	let vcs = parse_optional_table(table, "vcs", "pon.lock package vcs")?
		.map(LockedVcs::from_table)
		.transpose()?;

	Ok(LockedPackage { name, version, marker, kind, wheels, sdist, directory, vcs })
}

fn parse_wheels(table: &dyn TableLike) -> Result<Vec<LockedWheel>> {
	let Some(item) = table.get("wheels") else {
		return Ok(Vec::new());
	};
	let Some(wheels) = item.as_array_of_tables() else {
		return Err(Error::Cli("pon.lock package wheels must be an array of tables".to_owned()));
	};
	wheels
		.iter()
		.map(|wheel| LockedWheel::from_table(wheel))
		.collect::<Result<Vec<_>>>()
}

fn parse_optional_table<'a>(
	table: &'a dyn TableLike,
	key: &str,
	label: &str,
) -> Result<Option<&'a dyn TableLike>> {
	let Some(item) = table.get(key) else {
		return Ok(None);
	};
	item
		.as_table_like()
		.map(Some)
		.ok_or_else(|| Error::Cli(format!("{label} must be a table")))
}

fn parse_hashes(table: &dyn TableLike) -> Result<BTreeMap<String, String>> {
	let Some(item) = table.get("hashes") else {
		return Ok(BTreeMap::new());
	};
	let Some(hashes) = item.as_table_like() else {
		return Err(Error::Cli("pon.lock hashes must be a table".to_owned()));
	};
	let mut parsed = BTreeMap::new();
	for (algorithm, digest) in hashes.iter() {
		if digest.is_none() {
			continue;
		}
		let Some(digest) = digest.as_str() else {
			return Err(Error::Cli(format!("pon.lock hash `{}` must be a string", algorithm)));
		};
		parsed.insert(algorithm.to_owned(), digest.to_owned());
	}
	Ok(parsed)
}

fn required_string(table: &dyn TableLike, key: &str, message: &str) -> Result<String> {
	optional_string(table, key, message)?.ok_or_else(|| Error::Cli(message.to_owned()))
}

fn optional_string(table: &dyn TableLike, key: &str, label: &str) -> Result<Option<String>> {
	let Some(item) = table.get(key) else {
		return Ok(None);
	};
	item
		.as_str()
		.map(|value| Some(value.to_owned()))
		.ok_or_else(|| Error::Cli(format!("{label} must be a string")))
}

fn optional_bool(table: &dyn TableLike, key: &str, label: &str) -> Result<Option<bool>> {
	let Some(item) = table.get(key) else {
		return Ok(None);
	};
	item
		.as_bool()
		.map(Some)
		.ok_or_else(|| Error::Cli(format!("{label} must be a boolean")))
}

fn parse_kind(value: &str) -> Result<LockedPackageKind> {
	match value {
		"native" => Ok(LockedPackageKind::Native),
		"cabi-refused" => Ok(LockedPackageKind::CAbiRefused),
		"pure" => Ok(LockedPackageKind::Pure),
		_ => Err(Error::Cli(format!("unsupported pon package kind `{value}`"))),
	}
}

fn validate_lock_version(version: &str) -> Result<()> {
	let major = version.split_once('.').map_or(version, |(major, _)| major);
	if major == "1" {
		Ok(())
	} else {
		Err(Error::Cli(format!(
			"unsupported pon.lock lock-version `{version}`; expected major version 1"
		)))
	}
}

fn warn_unknown_top_level_keys(root: &dyn TableLike) {
	for (key, item) in root.iter() {
		if item.is_none() {
			continue;
		}
		match key {
			"lock-version" | "created-by" | "requires-python" | "tool" | "packages" => {},
			unknown => eprintln!("warning: ignoring unsupported pon.lock top-level key `{unknown}`"),
		}
	}
}

fn table_at<'a>(item: &'a Item, path: &[&str]) -> Option<&'a dyn TableLike> {
	let mut current = item;
	for key in path {
		current = current.as_table_like()?.get(key)?;
	}
	current.as_table_like()
}

fn table_at_table<'a>(table: &'a dyn TableLike, path: &[&str]) -> Option<&'a dyn TableLike> {
	let mut current = table.get(path.first()?)?;
	for key in &path[1..] {
		current = current.as_table_like()?.get(key)?;
	}
	current.as_table_like()
}

fn file_artifact_table(name: &str, url: &str, hashes: &BTreeMap<String, String>) -> Table {
	let mut table = Table::new();
	table.insert("name", value(name));
	table.insert("url", value(url));
	table.insert("hashes", value(hashes_inline_table(hashes)));
	table
}

fn hashes_inline_table(hashes: &BTreeMap<String, String>) -> InlineTable {
	let mut table = InlineTable::new();
	for (algorithm, digest) in hashes {
		table.insert(algorithm, Value::from(digest.as_str()));
	}
	table.fmt();
	table
}

fn sha256_hex(input: &[u8]) -> String {
	const HEX: &[u8; 16] = b"0123456789abcdef";
	let digest = Sha256::digest(input);
	let mut output = String::with_capacity(digest.len() * 2);
	for byte in digest {
		output.push(HEX[(byte >> 4) as usize] as char);
		output.push(HEX[(byte & 0x0f) as usize] as char);
	}
	output
}

#[cfg(test)]
mod tests {
	use super::*;

	fn unique_temp_dir(label: &str) -> PathBuf {
		let path = std::env::temp_dir().join(format!(
			"pon-lock-{label}-{}-{}",
			std::process::id(),
			std::thread::current().name().unwrap_or("unnamed")
		));
		let _ = fs::remove_dir_all(&path);
		fs::create_dir_all(&path).expect("create temp dir");
		path
	}

	#[test]
	fn serializes_pep_751_shaped_lock_with_tool_metadata_only_when_needed() {
		let lock = LockFile::with_input_hash(
			vec![
				LockedPackage::native("fastjson", "0.1.0"),
				LockedPackage::pure("idna", "3.10"),
				LockedPackage::cabi_refused("numpy", "2.3.1"),
			],
			">=3.14",
			"sha256:fixture",
		);
		let rendered = lock.to_string();

		assert!(rendered.contains("lock-version = \"1.0\""));
		assert!(rendered.contains("created-by = \"pon 0.1.0\""));
		assert!(rendered.contains("requires-python = \">=3.14\""));
		assert!(rendered.contains("[tool.pon]\ninput-hash = \"sha256:fixture\""));
		assert!(rendered.contains("[[packages]]\nname = \"fastjson\"\nversion = \"0.1.0\""));
		assert!(rendered.contains("[packages.tool.pon]\nkind = \"native\""));
		assert!(rendered.contains("[[packages]]\nname = \"idna\"\nversion = \"3.10\""));
		assert!(rendered.contains("[packages.tool.pon]\nkind = \"cabi-refused\""));
	}

	#[test]
	fn serializes_and_reads_artifact_tables() {
		let mut hashes = BTreeMap::new();
		hashes.insert("sha256".to_owned(), "abc123".to_owned());
		let mut package = LockedPackage::pure("idna", "3.10");
		package.marker = Some("python_version >= '3.14'".to_owned());
		package.wheels.push(LockedWheel {
			name: "idna-3.10-py3-none-any.whl".to_owned(),
			url: "https://files.example/idna.whl".to_owned(),
			hashes,
		});
		let lock = LockFile::with_input_hash(vec![package], ">=3.14", "sha256:fixture");

		let read_back = lock.to_string().parse::<LockFile>().expect("parse lock");

		assert_eq!(read_back, lock);
	}

	#[test]
	fn from_records_lists_full_transitive_graph_deterministically() {
		let records = vec![
			PackageRecord {
				name:    "pkg-a".to_owned(),
				version: "1.0.0".to_owned(),
				kind:    PackageKind::Pure,
			},
			PackageRecord {
				name:    "pkg-c".to_owned(),
				version: "1.0.0".to_owned(),
				kind:    PackageKind::Pure,
			},
			PackageRecord {
				name:    "pkg-b".to_owned(),
				version: "1.0.0".to_owned(),
				kind:    PackageKind::Pure,
			},
		];
		let first = LockFile::from_records(&records).to_string();
		let second = LockFile::from_records(&records).to_string();

		assert_eq!(first, second);
		assert!(first.contains("name = \"pkg-a\""));
		assert!(first.contains("name = \"pkg-b\""));
		assert!(first.contains("name = \"pkg-c\""));
		assert!(first.find("name = \"pkg-a\"") < first.find("name = \"pkg-b\""));
		assert!(first.find("name = \"pkg-b\"") < first.find("name = \"pkg-c\""));
	}

	#[test]
	fn input_hash_is_sorted_and_includes_tool_settings() {
		let left = compute_input_hash(["b>=1", "a==1"], Some("catalog:"), true);
		let right = compute_input_hash(["a==1", "b>=1"], Some("catalog:"), true);
		let different_settings = compute_input_hash(["a==1", "b>=1"], Some("catalog:"), false);

		assert_eq!(left, right);
		assert_ne!(left, different_settings);
		assert!(left.starts_with("sha256:"));
	}

	#[test]
	fn rejects_unsupported_major_version() {
		let lock = concat!(
			"lock-version = \"2.0\"\n",
			"created-by = \"pon 0.1.0\"\n",
			"requires-python = \">=3.14\"\n",
			"\n",
			"[tool.pon]\n",
			"input-hash = \"sha256:fixture\"\n",
		);

		let error = lock
			.parse::<LockFile>()
			.expect_err("major version should be rejected");

		assert!(
			error
				.to_string()
				.contains("unsupported pon.lock lock-version `2.0`")
		);
	}

	#[test]
	fn reads_written_lock_file() {
		let lock = LockFile::with_input_hash(
			vec![LockedPackage::pure("idna", "3.10"), LockedPackage::native("fastjson", "0.1.0")],
			">=3.14",
			"sha256:fixture",
		);
		let path = std::env::temp_dir().join(format!(
			"pon-lock-test-{}-{}.lock",
			std::process::id(),
			std::thread::current().name().unwrap_or("unnamed")
		));

		lock.write_to_path(&path).expect("write lock");
		let read_back = LockFile::read_from_path(&path).expect("read lock");
		let _ = fs::remove_file(path);

		assert_eq!(read_back, lock);
	}

	#[test]
	fn project_input_hash_uses_pyproject_settings_and_overrides() {
		let pyproject = PyProject::from_str(
			"pyproject.toml",
			r#"
[project]
dependencies = ["demo>=1", "idna==3.10"]

[tool.pon]
index-url = "catalog:"
allow-prerelease = true
"#,
		)
		.expect("parse pyproject");

		assert_eq!(
			compute_project_input_hash(&pyproject),
			compute_input_hash(["demo>=1", "idna==3.10"], Some("catalog:"), true)
		);
		assert_eq!(
			compute_project_input_hash_with_overrides(
				&pyproject,
				Some("https://example.test/simple/"),
				Some(false)
			),
			compute_input_hash(["idna==3.10", "demo>=1"], Some("https://example.test/simple/"), false)
		);
	}

	#[test]
	fn project_root_read_helpers_handle_missing_and_stale_frozen_locks() {
		let root = unique_temp_dir("project-root");
		let lock = LockFile::with_input_hash(
			vec![LockedPackage::pure("idna", "3.10")],
			">=3.14",
			"sha256:fresh",
		);

		assert!(
			LockFile::read_from_project_root(&root)
				.expect("missing optional lock")
				.is_none()
		);
		assert!(
			LockFile::read_for_install(&root, "sha256:fresh", false)
				.expect("missing non-frozen lock")
				.is_none()
		);
		let missing =
			LockFile::read_for_install(&root, "sha256:fresh", true).expect_err("missing frozen lock");
		assert!(missing.to_string().contains("pon.lock not found"));

		lock
			.write_to_project_root(&root)
			.expect("write project lock");
		assert_eq!(
			LockFile::read_frozen_for_install(&root, "sha256:fresh").expect("fresh frozen lock"),
			lock
		);
		assert!(
			LockFile::read_for_install(&root, "sha256:stale", false)
				.expect("stale non-frozen lock")
				.is_none()
		);
		let stale =
			LockFile::read_for_install(&root, "sha256:stale", true).expect_err("stale frozen lock");
		assert!(stale.to_string().contains("input-hash mismatch"));
		let _ = fs::remove_dir_all(root);
	}

	#[test]
	fn locked_packages_convert_to_current_install_records() {
		let root = unique_temp_dir("install-records");
		let cache_dir = root.join(".pon/cache");
		fs::create_dir_all(&cache_dir).expect("create cache");

		let mut directory_package = LockedPackage::pure("local-demo", "1.0");
		directory_package.directory =
			Some(LockedDirectory { path: PathBuf::from("vendor/local-demo"), editable: true });
		let directory_record = directory_package
			.to_resolved_record(&root, &cache_dir)
			.expect("directory record");
		assert_eq!(directory_record.artifact, PackageArtifact::Dir {
			path:     root.join("vendor/local-demo"),
			editable: true,
		});

		let wheel_bytes = b"wheel bytes";
		let wheel_path = root.join("demo-1.0-py3-none-any.whl");
		fs::write(&wheel_path, wheel_bytes).expect("write wheel bytes");
		let mut hashes = BTreeMap::new();
		hashes.insert("sha256".to_owned(), sha256_hex(wheel_bytes));
		let file_url = format!("file://{}", wheel_path.display());
		let mut wheel_package = LockedPackage::pure("demo", "1.0");
		wheel_package.wheels.push(LockedWheel {
			name:   "demo-1.0-py3-none-any.whl".to_owned(),
			url:    file_url.clone(),
			hashes: hashes.clone(),
		});

		let wheel_record = wheel_package
			.to_resolved_record(&root, &cache_dir)
			.expect("wheel record");
		let PackageArtifact::Wheel { path, direct_url } = wheel_record.artifact else {
			panic!("expected wheel artifact");
		};
		assert_eq!(path, wheel_path);
		let direct_url = direct_url.expect("direct URL");
		assert_eq!(direct_url.url, file_url);
		assert_eq!(direct_url.archive_info.expect("archive info").hashes, hashes);
		let _ = fs::remove_dir_all(root);
	}
}
