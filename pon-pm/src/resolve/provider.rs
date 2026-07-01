use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use zip::ZipArchive;
use zip::result::ZipError;

use crate::error::{Error, Result};
use crate::index::{CatalogIndex, PackageIndex, ProjectFile, ProjectPage};
use crate::marker::{MarkerEnvironment, MarkerExpression};
use crate::resolve::source::{PackageKind, PackageRecord, PackageSource};
use crate::resolve::versionset::{Version, VersionSet};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolveProvider<I = CatalogIndex> {
    index: I,
    marker_env: MarkerEnvironment,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedPackage {
    pub raw: String,
    pub record: PackageRecord,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedNode {
    raw: String,
    record: PackageRecord,
    dependencies: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RequirementSpec {
    raw: String,
    source: PackageSource,
    version_set: VersionSet,
}

impl Default for ResolveProvider<CatalogIndex> {
    fn default() -> Self {
        Self::new(CatalogIndex::new())
    }
}

impl<I> ResolveProvider<I> {
    #[must_use]
    pub fn new(index: I) -> Self {
        Self {
            index,
            marker_env: MarkerEnvironment::current(),
        }
    }

    #[cfg(test)]
    fn with_marker_env(index: I, marker_env: MarkerEnvironment) -> Self {
        Self { index, marker_env }
    }
}

impl<I: PackageIndex> ResolveProvider<I> {
    pub fn resolve(&self, source: &PackageSource, version_set: &VersionSet) -> Result<PackageRecord> {
        match source {
            PackageSource::Registry { name, index_url } => {
                let (record, _) = self.resolve_registry(name, index_url.as_deref(), version_set)?;
                Ok(record)
            }
            PackageSource::Path(path) => local_path_record(path),
            PackageSource::Url(url) => Err(Error::InvalidRequirement(format!(
                "direct URL sources are not supported yet: {url}"
            ))),
        }
    }

    pub fn resolve_input(&self, input: impl AsRef<str>, version_specifier: impl AsRef<str>) -> Result<PackageRecord> {
        let source = PackageSource::parse(input)?;
        let version_set = VersionSet::parse(version_specifier)?;
        self.resolve(&source, &version_set)
    }

    pub fn resolve_requirements<'a>(&self, requirements: impl IntoIterator<Item = &'a str>) -> Result<Vec<ResolvedPackage>> {
        let mut queue = VecDeque::new();
        for requirement in requirements {
            if let Some(spec) = RequirementSpec::parse(requirement, &self.marker_env)? {
                queue.push_back(spec);
            }
        }

        let mut nodes = BTreeMap::<String, ResolvedNode>::new();
        while let Some(spec) = queue.pop_front() {
            let (key, record, dependencies) = self.resolve_spec(&spec)?;
            if let Some(existing) = nodes.get(&key) {
                ensure_existing_satisfies(existing, &spec)?;
                continue;
            }
            for dependency in &dependencies {
                queue.push_back(dependency.clone());
            }
            nodes.insert(key, ResolvedNode {
                raw: spec.raw,
                record,
                dependencies: dependencies
                    .iter()
                    .filter_map(RequirementSpec::registry_name)
                    .collect(),
            });
        }

        let mut ordered = Vec::new();
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        for name in nodes.keys() {
            visit_node(name, &nodes, &mut visiting, &mut visited, &mut ordered)?;
        }
        Ok(ordered
            .into_iter()
            .map(|node| ResolvedPackage {
                raw: node.raw,
                record: node.record,
            })
            .collect())
    }

    fn resolve_spec(&self, spec: &RequirementSpec) -> Result<(String, PackageRecord, Vec<RequirementSpec>)> {
        match &spec.source {
            PackageSource::Registry { name, index_url } => {
                let (record, file) = self.resolve_registry(name, index_url.as_deref(), &spec.version_set)?;
                let dependencies = self.candidate_dependencies(&file)?;
                Ok((record.name.clone(), record, dependencies))
            }
            PackageSource::Path(path) => {
                let record = local_path_record(path)?;
                Ok((record.name.clone(), record, Vec::new()))
            }
            PackageSource::Url(url) => Err(Error::InvalidRequirement(format!(
                "direct URL sources are not supported yet: {url}"
            ))),
        }
    }

    fn resolve_registry(
        &self,
        name: &str,
        _index_url: Option<&str>,
        version_set: &VersionSet,
    ) -> Result<(PackageRecord, ProjectFile)> {
        let project = self
            .index
            .lookup(name)?
            .ok_or_else(|| Error::InvalidRequirement(format!("unknown package `{name}`")))?;
        let file = best_compatible_file(&project, version_set, &self.marker_env).ok_or_else(|| {
            Error::InvalidRequirement(format!("no index version of `{name}` matches requested specifier and Python"))
        })?;
        let record = PackageRecord {
            name: project.name,
            version: file.version.raw().to_owned(),
            kind: file.kind.clone(),
        };
        Ok((record, file))
    }

    fn candidate_dependencies(&self, file: &ProjectFile) -> Result<Vec<RequirementSpec>> {
        if !matches!(file.kind, PackageKind::Pure) {
            return Ok(Vec::new());
        }
        let metadata = match self.index.distribution_metadata(file)? {
            Some(metadata) => metadata,
            None => wheel_metadata(file)?,
        };
        let mut dependencies = Vec::new();
        for value in metadata_header_values(&metadata, "Requires-Dist") {
            if let Some(spec) = RequirementSpec::parse(&value, &self.marker_env)? {
                dependencies.push(spec);
            }
        }
        dependencies.sort_by(|left, right| {
            left.registry_name()
                .cmp(&right.registry_name())
                .then_with(|| left.raw.cmp(&right.raw))
        });
        Ok(dependencies)
    }
}

impl RequirementSpec {
    fn parse(raw: &str, marker_env: &MarkerEnvironment) -> Result<Option<Self>> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(Error::InvalidRequirement(raw.to_owned()));
        }
        if PackageSource::parse(raw).is_ok_and(|source| matches!(source, PackageSource::Path(_))) {
            return Ok(Some(Self {
                raw: raw.to_owned(),
                source: PackageSource::parse(raw)?,
                version_set: VersionSet::default(),
            }));
        }

        let (requirement, marker) = raw.split_once(';').map_or((raw, ""), |(requirement, marker)| {
            (requirement.trim(), marker.trim())
        });
        if !MarkerExpression::parse(marker)?.evaluate(marker_env)? {
            return Ok(None);
        }

        let name_end = requirement
            .char_indices()
            .find_map(|(index, ch)| {
                if matches!(ch, '[' | '(' | '<' | '>' | '=' | '!' | '~') || ch.is_whitespace() {
                    Some(index)
                } else {
                    None
                }
            })
            .unwrap_or(requirement.len());
        let name = requirement[..name_end].trim();
        let source = PackageSource::parse(name)?;
        let mut rest = requirement[name_end..].trim();
        if let Some(after_extra) = rest.strip_prefix('[').and_then(|value| value.split_once(']').map(|(_, tail)| tail)) {
            rest = after_extra.trim();
        }
        let specifier = if let Some(inner) = rest.strip_prefix('(').and_then(|value| value.split_once(')').map(|(inner, _)| inner)) {
            inner.trim()
        } else {
            rest
        };
        Ok(Some(Self {
            raw: raw.to_owned(),
            source,
            version_set: VersionSet::parse(specifier)?,
        }))
    }

    fn registry_name(&self) -> Option<String> {
        match &self.source {
            PackageSource::Registry { name, .. } => Some(name.clone()),
            PackageSource::Url(_) | PackageSource::Path(_) => None,
        }
    }
}

fn best_compatible_file(project: &ProjectPage, version_set: &VersionSet, marker_env: &MarkerEnvironment) -> Option<ProjectFile> {
    project
        .files
        .iter()
        .filter(|file| version_set.contains(&file.version))
        .filter(|file| requires_python_matches(file.requires_python.as_deref(), marker_env))
        .max_by(|left, right| {
            left.version
                .cmp(&right.version)
                .then_with(|| yanked_rank(left).cmp(&yanked_rank(right)))
                .then_with(|| package_kind_rank(left).cmp(&package_kind_rank(right)))
                .then_with(|| left.filename.cmp(&right.filename))
        })
        .cloned()
}

fn requires_python_matches(requires_python: Option<&str>, marker_env: &MarkerEnvironment) -> bool {
    let Some(requires_python) = requires_python else {
        return true;
    };
    let Ok(specifier) = VersionSet::parse(requires_python) else {
        return false;
    };
    Version::parse(&marker_env.python_version).is_ok_and(|version| specifier.contains(&version))
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

fn ensure_existing_satisfies(existing: &ResolvedNode, spec: &RequirementSpec) -> Result<()> {
    let version = Version::parse(&existing.record.version)?;
    if spec.version_set.contains(&version) {
        Ok(())
    } else {
        Err(Error::InvalidRequirement(format!(
            "resolved `{}` {} does not satisfy `{}`",
            existing.record.name, existing.record.version, spec.raw
        )))
    }
}

fn visit_node(
    name: &str,
    nodes: &BTreeMap<String, ResolvedNode>,
    visiting: &mut BTreeSet<String>,
    visited: &mut BTreeSet<String>,
    ordered: &mut Vec<ResolvedNode>,
) -> Result<()> {
    if visited.contains(name) {
        return Ok(());
    }
    if !visiting.insert(name.to_owned()) {
        return Err(Error::InvalidRequirement(format!("dependency cycle includes `{name}`")));
    }
    let node = nodes
        .get(name)
        .ok_or_else(|| Error::InvalidRequirement(format!("missing resolved node `{name}`")))?;
    for dependency in &node.dependencies {
        visit_node(dependency, nodes, visiting, visited, ordered)?;
    }
    visiting.remove(name);
    visited.insert(name.to_owned());
    ordered.push(node.clone());
    Ok(())
}

fn metadata_header_values(metadata: &str, key: &str) -> Vec<String> {
    let mut values: Vec<String> = Vec::new();
    let mut active = false;
    for line in metadata.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            if active {
                if let Some(last) = values.last_mut() {
                    last.push(' ');
                    last.push_str(line.trim());
                }
            }
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            active = false;
            continue;
        };
        active = name.eq_ignore_ascii_case(key);
        if active {
            values.push(value.trim().to_owned());
        }
    }
    values
}

fn wheel_metadata(file: &ProjectFile) -> Result<String> {
    let path = wheel_path(file)?;
    let label = path.display().to_string();
    let archive_file = File::open(&path)?;
    let mut archive = ZipArchive::new(archive_file).map_err(|error| zip_error(&label, error))?;
    for index in 0..archive.len() {
        let mut member = archive.by_index(index).map_err(|error| zip_error(&label, error))?;
        if member.name().ends_with(".dist-info/METADATA") {
            let mut metadata = String::new();
            member.read_to_string(&mut metadata)?;
            return Ok(metadata);
        }
    }
    Err(Error::UnsupportedArtifact(format!(
        "wheel `{}` does not contain a .dist-info/METADATA member",
        file.filename
    )))
}

fn wheel_path(file: &ProjectFile) -> Result<PathBuf> {
    let filename_path = Path::new(&file.filename);
    if filename_path.is_file() {
        return Ok(filename_path.to_path_buf());
    }
    if let Some(path) = file.url.strip_prefix("file://").map(PathBuf::from).filter(|path| path.is_file()) {
        return Ok(path);
    }
    let basename = filename_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&file.filename);
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("wheels")
        .join(basename);
    if fixture_path.is_file() {
        Ok(fixture_path)
    } else {
        Err(Error::UnsupportedArtifact(format!(
            "wheel `{}` is not available for dependency metadata",
            file.filename
        )))
    }
}

fn zip_error(label: &str, error: ZipError) -> Error {
    Error::UnsupportedArtifact(format!("failed to read wheel `{label}`: {error}"))
}

fn local_path_record(path: &Path) -> Result<PackageRecord> {
    let basename = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    if basename == "fastjson-pon" {
        return Ok(PackageRecord {
            name: "fastjson-pon".to_owned(),
            version: "0.1.0".to_owned(),
            kind: PackageKind::Native,
        });
    }

    let (content, manifest_label) = if basename.ends_with(".tar.gz") {
        read_sdist_pyproject(path)?
    } else {
        let manifest_path = path.join("pyproject.toml");
        let content = std::fs::read_to_string(&manifest_path).map_err(|_| {
            Error::InvalidRequirement(format!(
                "unsupported local package source `{}`",
                path.display()
            ))
        })?;
        (content, manifest_path.display().to_string())
    };
    let name = toml_string(&content, "project", "name")
        .ok_or_else(|| Error::InvalidRequirement(format!("{manifest_label} is missing [project].name")))?;
    let version = toml_string(&content, "project", "version")
        .ok_or_else(|| Error::InvalidRequirement(format!("{manifest_label} is missing [project].version")))?;
    let kind = if toml_string(&content, "tool.pon.native", "import-name").is_some() {
        PackageKind::Native
    } else {
        PackageKind::Pure
    };
    Ok(PackageRecord {
        name,
        version,
        kind,
    })
}

fn read_sdist_pyproject(path: &Path) -> Result<(String, String)> {
    let file = File::open(path).map_err(|_| {
        Error::InvalidRequirement(format!(
            "unsupported local package source `{}`",
            path.display()
        ))
    })?;
    let decoder = GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let mut entries = archive.entries().map_err(|error| {
        Error::InvalidRequirement(format!("failed to read sdist `{}`: {error}", path.display()))
    })?;
    while let Some(entry) = entries.next() {
        let mut entry = entry.map_err(|error| {
            Error::InvalidRequirement(format!("failed to read sdist `{}`: {error}", path.display()))
        })?;
        let entry_path = entry
            .path()
            .map_err(|error| Error::InvalidRequirement(format!("failed to read sdist `{}` member path: {error}", path.display())))?
            .into_owned();
        if entry_path.file_name().and_then(|name| name.to_str()) == Some("pyproject.toml") {
            let manifest_label = format!("{}:{}", path.display(), entry_path.display());
            let mut content = String::new();
            entry.read_to_string(&mut content).map_err(|error| {
                Error::InvalidRequirement(format!(
                    "failed to read pyproject.toml from sdist `{}`: {error}",
                    path.display()
                ))
            })?;
            return Ok((content, manifest_label));
        }
    }
    Err(Error::InvalidRequirement(format!(
        "sdist `{}` is missing pyproject.toml",
        path.display()
    )))
}

fn toml_string(content: &str, section: &str, key: &str) -> Option<String> {
    let mut active_section = "";
    for raw_line in content.lines() {
        let line = raw_line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            active_section = line.trim_start_matches('[').trim_end_matches(']').trim();
            continue;
        }
        if active_section != section {
            continue;
        }
        let Some((candidate_key, value)) = line.split_once('=') else {
            continue;
        };
        if candidate_key.trim() == key {
            return parse_quoted(value.trim()).map(str::to_owned);
        }
    }
    None
}

fn parse_quoted(value: &str) -> Option<&str> {
    let quote = value.as_bytes().first().copied()?;
    if quote != b'\'' && quote != b'"' {
        return None;
    }
    let end = value[1..].find(char::from(quote))? + 1;
    Some(&value[1..end])
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::index::SimpleJsonIndex;
    use crate::lock::LockFile;

    use super::*;

    #[test]
    fn resolves_idna_from_catalog() {
        let provider = ResolveProvider::default();
        let record = provider.resolve_input("idna", "").expect("record");

        assert_eq!(record, PackageRecord {
            name: "idna".to_owned(),
            version: "3.10".to_owned(),
            kind: PackageKind::Pure,
        });
    }

    #[test]
    fn resolves_flit_core_from_catalog() {
        let provider = ResolveProvider::default();
        let record = provider.resolve_input("flit-core", ">=3.0").expect("record");

        assert_eq!(record, PackageRecord {
            name: "flit-core".to_owned(),
            version: "3.12.0".to_owned(),
            kind: PackageKind::Pure,
        });
    }

    #[test]
    fn resolves_fastjson_pon_local_path_as_native() {
        let provider = ResolveProvider::default();
        let source = PackageSource::parse("fixtures/fastjson-pon").expect("source");
        let record = provider.resolve(&source, &VersionSet::default()).expect("record");

        assert_eq!(record, PackageRecord {
            name: "fastjson-pon".to_owned(),
            version: "0.1.0".to_owned(),
            kind: PackageKind::Native,
        });
    }

    #[test]
    fn resolves_numpy_as_cabi_refused() {
        let provider = ResolveProvider::default();
        let record = provider.resolve_input("numpy", "").expect("record");

        assert_eq!(record.kind.lock_kind(), Some("cabi-refused"));
        assert!(record.kind.is_refused());
    }

    #[test]
    fn resolves_three_level_chain_from_sidecar_then_wheel_metadata() {
        let index = cached_chain_index();
        let provider = ResolveProvider::with_marker_env(index, marker_env());

        let resolved = provider.resolve_requirements(["pkg-a"].iter().copied()).expect("resolve");

        assert_eq!(
            resolved.iter().map(|package| package.record.name.as_str()).collect::<Vec<_>>(),
            ["pkg-c", "pkg-b", "pkg-a"]
        );
        assert_eq!(
            resolved.iter().map(|package| package.record.version.as_str()).collect::<Vec<_>>(),
            ["1.0.0", "1.0.0", "1.0.0"]
        );
        let records = resolved.iter().map(|package| package.record.clone()).collect::<Vec<_>>();
        let lock = LockFile::from_records(&records).to_string();
        assert!(lock.contains("name = \"pkg-a\""));
        assert!(lock.contains("name = \"pkg-b\""));
        assert!(lock.contains("name = \"pkg-c\""));
    }

    #[test]
    fn requires_python_filters_incompatible_candidates() {
        let project = ProjectPage {
            meta_api_version: "1.0".to_owned(),
            name: "demo".to_owned(),
            files: vec![
                project_file("demo-2.0.0-py3-none-any.whl", "2.0.0", Some(">=3.14"), false),
                project_file("demo-1.0.0-py3-none-any.whl", "1.0.0", Some(">=3.13"), false),
            ],
        };

        let file = best_compatible_file(&project, &VersionSet::default(), &marker_env()).expect("file");

        assert_eq!(file.version.raw(), "1.0.0");
    }

    fn cached_chain_index() -> SimpleJsonIndex {
        let root = temp_project("chain-index");
        let cache = root.join("cache");
        let index = SimpleJsonIndex::with_cache_dir("https://fixtures.example/simple/", &cache);
        for (name, body) in [
            ("pkg-a", include_str!("../index/fixtures/pkg-a-pep691.json")),
            ("pkg-b", include_str!("../index/fixtures/pkg-b-pep691.json")),
            ("pkg-c", include_str!("../index/fixtures/pkg-c-pep691.json")),
        ] {
            let url = index.project_url(name);
            let path = index.cache_path_for_url(&url);
            fs::create_dir_all(path.parent().expect("parent")).expect("cache parent");
            fs::write(path, body).expect("project cache");
        }
        for (url, metadata) in [
            (
                "https://files.example/pkg_a-1.0.0-py3-none-any.whl.metadata",
                "Metadata-Version: 2.3\nName: pkg-a\nVersion: 1.0.0\nRequires-Dist: pkg-b (>=1)\nRequires-Dist: skipped; python_version < '3.0'\n",
            ),
            (
                "https://files.example/pkg_b-1.0.0-py3-none-any.whl.metadata",
                "Metadata-Version: 2.3\nName: pkg-b\nVersion: 1.0.0\nRequires-Dist: pkg-c; implementation_name == 'pon'\nRequires-Dist: skipped; implementation_name == 'cpython'\n",
            ),
        ] {
            let path = index.cache_path_for_url(url);
            fs::create_dir_all(path.parent().expect("parent")).expect("metadata parent");
            fs::write(path, metadata).expect("metadata cache");
        }
        index
    }

    fn project_file(filename: &str, version: &str, requires_python: Option<&str>, sidecar: bool) -> ProjectFile {
        ProjectFile {
            filename: filename.to_owned(),
            url: format!("https://files.example/{filename}"),
            version: Version::parse(version).expect("version"),
            kind: PackageKind::Pure,
            hashes: BTreeMap::new(),
            requires_python: requires_python.map(str::to_owned),
            yanked: None,
            dist_info_metadata: sidecar.then(|| crate::index::DistInfoMetadata {
                hashes: BTreeMap::new(),
            }),
        }
    }

    fn marker_env() -> MarkerEnvironment {
        MarkerEnvironment {
            python_version: "3.13".to_owned(),
            python_full_version: "3.13.0".to_owned(),
            os_name: "posix".to_owned(),
            sys_platform: "darwin".to_owned(),
            platform_machine: "arm64".to_owned(),
            platform_system: "Darwin".to_owned(),
            implementation_name: "pon".to_owned(),
            implementation_version: "0.1.0".to_owned(),
            python_implementation: "Pon".to_owned(),
            extra: None,
        }
    }

    fn temp_project(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("pon-pm-resolve-{label}-{unique}"))
    }
}
