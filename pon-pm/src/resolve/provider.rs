use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use flate2::read::GzDecoder;
use pep440_rs::{Operator, Version, VersionSpecifiers};
use pep508_rs::{ExtraName, MarkerEnvironment, Requirement, VersionOrUrl};
use pubgrub::{
    DefaultStringReporter, Dependencies, DependencyConstraints, DependencyProvider,
    PackageResolutionStatistics, PubGrubError, Ranges, Reporter, resolve as pubgrub_resolve,
};
use zip::ZipArchive;
use zip::result::ZipError;

use crate::error::{Error, Result};
use crate::index::{CatalogIndex, PackageIndex, ProjectFile};
use crate::marker::pon_marker_env;
use crate::metadata::CoreMetadata;
use crate::names;
use crate::pyproject::PyProject;
use crate::requirement::{RequirementInput, parse_requirement_input};
use crate::resolve::package::PonPackage;
use crate::resolve::source::{ArtifactSet, CandidateSource, IndexSource, PackageKind, PackageRecord};
use crate::resolve::versionset::range_from_specifiers;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolveProvider<I = CatalogIndex> {
    index: I,
    marker_env: MarkerEnvironment,
    allow_prerelease: bool,
    constraints: ConstraintSet,
    no_deps: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedPackage {
    pub raw: String,
    pub record: PackageRecord,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Resolution {
    pub dists: Vec<ResolvedDist>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedDist {
    pub name: String,
    pub version: Version,
    pub kind: PackageKind,
    pub artifact: ResolvedArtifact,
    pub dependencies: Vec<String>,
    pub marker: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedArtifact {
    Wheel(ProjectFile),
    Sdist(ProjectFile),
    Dir { path: PathBuf, editable: bool },
    Vcs {
        url: String,
        requested_rev: Option<String>,
        commit: String,
        dir: PathBuf,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ResolvedInput {
    Registry { raw: String, requirement: Requirement },
    Pinned { raw: String, candidate: PinnedCandidate },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PinnedCandidate {
    name: String,
    version: Version,
    kind: PackageKind,
    artifact: PinnedArtifact,
    dependencies: Vec<Requirement>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PinnedArtifact {
    Dir { path: PathBuf, editable: bool },
    Sdist { path: PathBuf },
    Vcs {
        url: String,
        requested_rev: Option<String>,
        commit: String,
        dir: PathBuf,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConstraintSet {
    constraints: HashMap<String, Vec<VersionSpecifiers>>,
}

impl ConstraintSet {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_requirements<T, R>(requirements: T) -> Result<Self>
    where
        T: IntoIterator<Item = R>,
        R: AsRef<str>,
    {
        let mut constraints = Self::new();
        let marker_env = pon_marker_env();
        for requirement in requirements {
            constraints.insert_requirement_with_env(requirement.as_ref(), &marker_env)?;
        }
        Ok(constraints)
    }

    pub fn from_requirements_with_env<T, R>(requirements: T, marker_env: &MarkerEnvironment) -> Result<Self>
    where
        T: IntoIterator<Item = R>,
        R: AsRef<str>,
    {
        let mut constraints = Self::new();
        for requirement in requirements {
            constraints.insert_requirement_with_env(requirement.as_ref(), marker_env)?;
        }
        Ok(constraints)
    }

    pub fn insert_requirement(&mut self, raw: impl AsRef<str>) -> Result<()> {
        let marker_env = pon_marker_env();
        self.insert_requirement_with_env(raw, &marker_env)
    }

    pub fn insert_requirement_with_env(&mut self, raw: impl AsRef<str>, marker_env: &MarkerEnvironment) -> Result<()> {
        let raw = raw.as_ref().trim();
        let input = parse_requirement_input(raw)?;
        self.insert_input_with_env(&input, raw, marker_env)
    }

    pub fn insert_input(&mut self, input: &RequirementInput, line: impl fmt::Display) -> Result<()> {
        let marker_env = pon_marker_env();
        self.insert_input_inner(input, line.to_string(), Some(&marker_env))
    }

    pub fn insert_input_with_env(
        &mut self,
        input: &RequirementInput,
        line: impl fmt::Display,
        marker_env: &MarkerEnvironment,
    ) -> Result<()> {
        self.insert_input_inner(input, line.to_string(), Some(marker_env))
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.constraints.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.constraints.values().map(Vec::len).sum()
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&[VersionSpecifiers]> {
        self.constraints.get(&names::normalize(name)).map(Vec::as_slice)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &[VersionSpecifiers])> {
        self.constraints
            .iter()
            .map(|(name, specifiers)| (name, specifiers.as_slice()))
    }

    fn insert_input_inner(
        &mut self,
        input: &RequirementInput,
        line: String,
        marker_env: Option<&MarkerEnvironment>,
    ) -> Result<()> {
        let RequirementInput::Pep508(requirement) = input else {
            return Err(invalid_constraint_line(&line));
        };

        if !requirement.extras.is_empty() {
            return Err(invalid_constraint_line(&line));
        }

        if let Some(marker_env) = marker_env {
            if !requirement.evaluate_markers(marker_env, &[]) {
                return Ok(());
            }
        }

        let specifiers = match &requirement.version_or_url {
            Some(VersionOrUrl::VersionSpecifier(specifiers)) => specifiers.clone(),
            Some(VersionOrUrl::Url(_)) => return Err(invalid_constraint_line(&line)),
            None => VersionSpecifiers::default(),
        };

        self.constraints
            .entry(names::normalize(requirement.name.as_ref()))
            .or_default()
            .push(specifiers);
        Ok(())
    }
}

impl From<HashMap<String, VersionSpecifiers>> for ConstraintSet {
    fn from(constraints: HashMap<String, VersionSpecifiers>) -> Self {
        Self {
            constraints: constraints
                .into_iter()
                .map(|(name, specifiers)| (names::normalize(&name), vec![specifiers]))
                .collect(),
        }
    }
}

fn invalid_constraint_line(line: &str) -> Error {
    Error::InvalidRequirement(format!("constraints cannot use extras or URLs: {line}"))
}

pub struct PonProvider<'a, S: CandidateSource> {
    pub source: &'a S,
    pub markers: MarkerEnvironment,
    pub allow_prerelease: bool,
    root_reqs: Vec<ResolvedInput>,
    pinned: std::cell::RefCell<HashMap<String, PinnedCandidate>>,
    pub constraints: ConstraintSet,
    pub rejects: std::cell::RefCell<BTreeMap<(String, String), Vec<String>>>,
    versions: std::cell::RefCell<HashMap<(String, bool), Vec<Version>>>,
    pub no_deps: bool,
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
            marker_env: pon_marker_env(),
            allow_prerelease: false,
            constraints: ConstraintSet::default(),
            no_deps: false,
        }
    }

    #[must_use]
    pub fn with_allow_prerelease(mut self, allow_prerelease: bool) -> Self {
        self.allow_prerelease = allow_prerelease;
        self
    }

    pub fn set_allow_prerelease(&mut self, allow_prerelease: bool) {
        self.allow_prerelease = allow_prerelease;
    }

    #[must_use]
    pub fn with_no_deps(mut self, no_deps: bool) -> Self {
        self.no_deps = no_deps;
        self
    }

    pub fn set_no_deps(&mut self, no_deps: bool) {
        self.no_deps = no_deps;
    }

    #[must_use]
    pub fn with_constraint_set(mut self, constraints: impl Into<ConstraintSet>) -> Self {
        self.constraints = constraints.into();
        self
    }

    #[must_use]
    pub fn with_constraints(self, constraints: impl Into<ConstraintSet>) -> Self {
        self.with_constraint_set(constraints)
    }

    pub fn set_constraint_set(&mut self, constraints: impl Into<ConstraintSet>) {
        self.constraints = constraints.into();
    }

    pub fn set_constraints(&mut self, constraints: impl Into<ConstraintSet>) {
        self.set_constraint_set(constraints);
    }

    #[cfg(test)]
    fn with_marker_env(index: I, marker_env: MarkerEnvironment) -> Self {
        Self {
            index,
            marker_env,
            allow_prerelease: false,
            constraints: ConstraintSet::default(),
            no_deps: false,
        }
    }

    fn configure_provider<'a, S: CandidateSource>(&self, provider: PonProvider<'a, S>) -> PonProvider<'a, S> {
        provider
            .with_allow_prerelease(self.allow_prerelease)
            .with_constraints(self.constraints.clone())
            .with_no_deps(self.no_deps)
    }
}

impl<I: PackageIndex> ResolveProvider<I> {
    pub fn resolve_input(&self, input: impl AsRef<str>, version_specifier: impl AsRef<str>) -> Result<PackageRecord> {
        let raw = input.as_ref();
        let mut resolved = resolved_input_from_raw(raw, &self.marker_env)?
            .ok_or_else(|| Error::InvalidRequirement(raw.trim().to_owned()))?;
        apply_legacy_version_specifier(&mut resolved, raw, version_specifier.as_ref())?;

        let root_names = [resolved.dist_name().to_owned()].into_iter().collect::<BTreeSet<_>>();
        let source = IndexSource::new(&self.index);
        let provider = self.configure_provider(PonProvider::from_resolved_inputs(
            &source,
            self.marker_env.clone(),
            vec![resolved],
        ));
        let resolution = match resolve_root(&provider) {
            Ok(resolution) => resolution,
            Err(error) => return Err(cabi_refusal_for_roots(&provider, &root_names).unwrap_or(error)),
        };
        resolution
            .dists
            .into_iter()
            .find(|dist| root_names.contains(&dist.name))
            .map(|dist| PackageRecord {
                name: dist.name,
                version: dist.version.to_string(),
                kind: dist.kind,
            })
            .ok_or_else(|| Error::InvalidRequirement(format!("no package was resolved for `{raw}`")))
    }

    pub fn resolve_requirements<'a>(
        &self,
        requirements: impl IntoIterator<Item = &'a str>,
    ) -> Result<Vec<ResolvedPackage>> {
        let source = IndexSource::new(&self.index);
        let provider = self.configure_provider(PonProvider::from_requirements_with_env(
            &source,
            requirements,
            self.marker_env.clone(),
        )?);
        let root_names = provider.root_dist_names();
        let resolution = match resolve_root(&provider) {
            Ok(resolution) => resolution,
            Err(error) => return Err(cabi_refusal_for_roots(&provider, &root_names).unwrap_or(error)),
        };
        Ok(resolution
            .dists
            .into_iter()
            .map(|dist| ResolvedPackage {
                raw: provider.raw_for_dist(&dist.name),
                record: PackageRecord {
                    name: dist.name,
                    version: dist.version.to_string(),
                    kind: dist.kind,
                },
            })
            .collect())
    }
}

impl<'a, S: CandidateSource> PonProvider<'a, S> {
    pub fn from_requirements<T, R>(source: &'a S, requirements: T) -> Result<Self>
    where
        T: IntoIterator<Item = R>,
        R: AsRef<str>,
    {
        Self::from_requirements_with_env(source, requirements, pon_marker_env())
    }

    fn from_requirements_with_env<T, R>(
        source: &'a S,
        requirements: T,
        markers: MarkerEnvironment,
    ) -> Result<Self>
    where
        T: IntoIterator<Item = R>,
        R: AsRef<str>,
    {
        let mut root_reqs = Vec::new();
        for requirement in requirements {
            let raw = requirement.as_ref();
            if let Some(input) = resolved_input_from_raw(raw, &markers)? {
                root_reqs.push(input);
            }
        }
        Ok(Self::from_resolved_inputs(source, markers, root_reqs))
    }

    fn from_resolved_inputs(source: &'a S, markers: MarkerEnvironment, root_reqs: Vec<ResolvedInput>) -> Self {
        let pinned = root_reqs
            .iter()
            .filter_map(|input| match input {
                ResolvedInput::Pinned { candidate, .. } => Some((candidate.name.clone(), candidate.clone())),
                ResolvedInput::Registry { .. } => None,
            })
            .collect::<HashMap<_, _>>();

        Self {
            source,
            markers,
            allow_prerelease: false,
            root_reqs,
            pinned: std::cell::RefCell::new(pinned),
            constraints: ConstraintSet::default(),
            rejects: std::cell::RefCell::new(BTreeMap::new()),
            versions: std::cell::RefCell::new(HashMap::new()),
            no_deps: false,
        }
    }

    #[must_use]
    pub fn with_allow_prerelease(mut self, allow_prerelease: bool) -> Self {
        self.allow_prerelease = allow_prerelease;
        self
    }

    pub fn set_allow_prerelease(&mut self, allow_prerelease: bool) {
        self.allow_prerelease = allow_prerelease;
    }

    #[must_use]
    pub fn with_no_deps(mut self, no_deps: bool) -> Self {
        self.no_deps = no_deps;
        self
    }

    pub fn set_no_deps(&mut self, no_deps: bool) {
        self.no_deps = no_deps;
    }

    #[must_use]
    pub fn with_constraint_set(mut self, constraints: impl Into<ConstraintSet>) -> Self {
        self.constraints = constraints.into();
        self
    }

    #[must_use]
    pub fn with_constraints(self, constraints: impl Into<ConstraintSet>) -> Self {
        self.with_constraint_set(constraints)
    }

    pub fn set_constraint_set(&mut self, constraints: impl Into<ConstraintSet>) {
        self.constraints = constraints.into();
    }

    pub fn set_constraints(&mut self, constraints: impl Into<ConstraintSet>) {
        self.set_constraint_set(constraints);
    }

    fn dependency_edges(&self, package: &PonPackage, version: &Version) -> Result<DependencyConstraints<PonPackage, Ranges<Version>>> {
        let mut edges = Vec::new();
        match package {
            PonPackage::Root => {
                for input in &self.root_reqs {
                    match input {
                        ResolvedInput::Registry { requirement, .. } => {
                            self.add_requirement_edges(&mut edges, requirement, &[])?;
                        }
                        ResolvedInput::Pinned { candidate, .. } => {
                            let candidates = vec![candidate.version.clone()];
                            let range = self.apply_constraints(
                                &candidate.name,
                                Ranges::singleton(candidate.version.clone()),
                                &candidates,
                            );
                            edges.push((PonPackage::Dist(candidate.name.clone()), range));
                        }
                    }
                }
            }
            PonPackage::Dist(name) => {
                if self.no_deps {
                    return Ok(DependencyConstraints::default());
                }
                for requirement in self.metadata_for_dist(name, version)?.requires_dist {
                    self.add_requirement_edges(&mut edges, &requirement, &[])?;
                }
            }
            PonPackage::Extra(name, extra) => {
                edges.push((PonPackage::Dist(name.clone()), Ranges::singleton(version.clone())));
                if !self.no_deps {
                    let extra = ExtraName::from_str(extra).map_err(|_| Error::InvalidRequirement(extra.clone()))?;
                    for requirement in self.metadata_for_dist(name, version)?.requires_dist {
                        self.add_requirement_edges(&mut edges, &requirement, &[extra.clone()])?;
                    }
                }
            }
        }
        Ok(edges.into_iter().collect())
    }

    fn add_requirement_edges(
        &self,
        edges: &mut Vec<(PonPackage, Ranges<Version>)>,
        requirement: &Requirement,
        extras: &[ExtraName],
    ) -> Result<()> {
        if !requirement.evaluate_markers(&self.markers, extras) {
            return Ok(());
        }

        if let Some(VersionOrUrl::Url(url)) = &requirement.version_or_url {
            let url = url.to_string();
            if is_git_direct_url(&url) {
                let candidate = self.ensure_pinned_git_requirement(requirement, &url)?;
                let candidates = vec![candidate.version.clone()];
                let range = self.apply_constraints(
                    &candidate.name,
                    Ranges::singleton(candidate.version.clone()),
                    &candidates,
                );

                let name = requirement.name.as_ref().to_owned();
                edges.push((PonPackage::Dist(name.clone()), range.clone()));
                for extra in &requirement.extras {
                    edges.push((PonPackage::Extra(name.clone(), extra.to_string()), range.clone()));
                }
                return Ok(());
            }
        }

        let name = requirement.name.as_ref().to_owned();
        let candidates = self.versions_for_name(&name)?;
        let range = self.apply_constraints(
            &name,
            self.range_for_requirement(requirement, &candidates)?,
            &candidates,
        );

        edges.push((PonPackage::Dist(name.clone()), range.clone()));
        for extra in &requirement.extras {
            edges.push((PonPackage::Extra(name.clone(), extra.to_string()), range.clone()));
        }
        Ok(())
    }

    fn range_for_requirement(&self, requirement: &Requirement, candidates: &[Version]) -> Result<Ranges<Version>> {
        let specifiers = match &requirement.version_or_url {
            Some(VersionOrUrl::VersionSpecifier(specifiers)) => specifiers.clone(),
            Some(VersionOrUrl::Url(url)) => {
                let url = url.to_string();
                if is_git_direct_url(&url) {
                    let candidate = self.ensure_pinned_git_requirement(requirement, &url)?;
                    return Ok(Ranges::singleton(candidate.version));
                }
                return Err(unsupported_direct_url(&url));
            }
            None => VersionSpecifiers::default(),
        };
        Ok(range_from_specifiers(&specifiers, candidates, self.allow_prerelease))
    }

    fn apply_constraints(
        &self,
        name: &str,
        mut range: Ranges<Version>,
        candidates: &[Version],
    ) -> Ranges<Version> {
        if let Some(constraints) = self.constraints.get(name) {
            for constraint in constraints {
                range = range.intersection(&range_from_specifiers(
                    constraint,
                    candidates,
                    self.allow_prerelease,
                ));
            }
        }
        range
    }

    fn metadata_for_dist(&self, name: &str, version: &Version) -> Result<CoreMetadata> {
        if let Some(candidate) = self.pinned_candidate(name).filter(|candidate| &candidate.version == version) {
            return Ok(core_metadata_from_pinned(&candidate));
        }

        let metadata = self.source.metadata(name, version)?;
        if let Some(requires_python) = metadata.requires_python.as_ref() {
            if !requires_python.contains(&self.markers.python_version().version) {
                return Err(Error::UnsupportedArtifact(format!(
                    "{name} {version} requires Python {requires_python}; pon is 3.14"
                )));
            }
        }
        Ok(metadata)
    }

    fn versions_for_package(&self, package: &PonPackage) -> Result<Vec<Version>> {
        match package {
            PonPackage::Root => Ok(vec![root_version()]),
            PonPackage::Dist(name) | PonPackage::Extra(name, _) => self.versions_for_name(name),
        }
    }

    fn versions_for_name(&self, name: &str) -> Result<Vec<Version>> {
        if let Some(candidate) = self.pinned_candidate(name) {
            return Ok(vec![candidate.version.clone()]);
        }

        let normalized = names::normalize(name);
        let include_yanked = self.root_requires_exact(&normalized);
        let key = (normalized.clone(), include_yanked);
        if let Some(versions) = self.versions.borrow().get(&key) {
            return Ok(versions.clone());
        }

        let versions = self.source.available_versions(&normalized, include_yanked)?;
        self.versions.borrow_mut().insert(key, versions.clone());
        Ok(versions)
    }

    fn root_requires_exact(&self, name: &str) -> bool {
        self.root_reqs.iter().any(|input| match input {
            ResolvedInput::Registry { requirement, .. } => {
                requirement.name.as_ref() == name
                    && requirement
                        .version_or_url
                        .as_ref()
                        .and_then(|version_or_url| match version_or_url {
                            VersionOrUrl::VersionSpecifier(specifiers) => Some(is_exact_equal_specifier(specifiers)),
                            VersionOrUrl::Url(_) => None,
                        })
                        .unwrap_or(false)
            }
            ResolvedInput::Pinned { candidate, .. } => candidate.name == name,
        })
    }

    fn pinned_candidate(&self, name: &str) -> Option<PinnedCandidate> {
        let normalized = names::normalize(name);
        self.pinned.borrow().get(&normalized).cloned()
    }

    fn ensure_pinned_git_requirement(&self, requirement: &Requirement, url: &str) -> Result<PinnedCandidate> {
        let requested_name = names::normalize(requirement.name.as_ref());
        if let Some(candidate) = self.pinned.borrow().get(&requested_name).cloned() {
            return Ok(candidate);
        }

        let candidate = pin_git_url(url)?;
        validate_pinned_requirement_name(&candidate, requirement)?;
        self.pinned.borrow_mut().insert(candidate.name.clone(), candidate.clone());
        Ok(candidate)
    }

    fn selected_artifact(&self, name: &str, version: &Version) -> Result<(ResolvedArtifact, PackageKind)> {
        if let Some(candidate) = self.pinned_candidate(name).filter(|candidate| &candidate.version == version) {
            return Ok((candidate.artifact.to_resolved(version, &candidate.kind), candidate.kind.clone()));
        }

        let artifacts = self.source.artifacts(name, version)?;
        let has_installable_sdist = source_distribution_allowed(&artifacts);
        if let Some(file) = artifacts.wheels.into_iter().find(|file| !file.kind.is_refused()) {
            let kind = file.kind.clone();
            return Ok((ResolvedArtifact::Wheel(file), kind));
        }
        if has_installable_sdist && let Some(file) = artifacts.sdist {
            let kind = file.kind.clone();
            return Ok((ResolvedArtifact::Sdist(file), kind));
        }
        Err(Error::UnsupportedArtifact(format!(
            "no installable artifact is available for `{name}` {version}"
        )))
    }

    fn record_rejects(&self, name: &str, version: &Version, artifacts: &ArtifactSet) {
        let mut reasons = artifacts
            .wheels
            .iter()
            .filter_map(|file| match &file.kind {
                PackageKind::CAbiRefused { reason } => Some(reason.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        if reasons.is_empty() {
            reasons.push("no pure Python wheel or source distribution is installable by pon".to_owned());
        }

        let key = (names::normalize(name), version.to_string());
        let mut rejects = self.rejects.borrow_mut();
        let entry = rejects.entry(key).or_default();
        for reason in reasons {
            if !entry.contains(&reason) {
                entry.push(reason);
            }
        }
    }

    fn reject_report_lines(&self, packages: &BTreeSet<String>) -> Vec<String> {
        self.rejects
            .borrow()
            .iter()
            .filter(|((name, _), _)| packages.is_empty() || packages.contains(name))
            .flat_map(|((name, version), reasons)| {
                reasons
                    .iter()
                    .map(move |reason| format!("  {name} {version}: {reason}"))
            })
            .collect()
    }

    fn root_dist_names(&self) -> BTreeSet<String> {
        self.root_reqs
            .iter()
            .map(|input| names::normalize(input.dist_name()))
            .collect()
    }

    fn raw_for_dist(&self, name: &str) -> String {
        let normalized = names::normalize(name);
        self.root_reqs
            .iter()
            .find_map(|input| match input {
                ResolvedInput::Registry { raw, requirement } if requirement.name.as_ref() == normalized => Some(raw.clone()),
                ResolvedInput::Pinned { raw, candidate } if candidate.name == normalized => Some(raw.clone()),
                _ => None,
            })
            .unwrap_or_else(|| normalized)
    }
}

fn source_distribution_allowed(artifacts: &ArtifactSet) -> bool {
    artifacts.sdist.is_some() && artifacts.wheels.iter().all(|file| matches!(file.kind, PackageKind::Pure))
}

fn cabi_refusal_for_roots<S: CandidateSource>(provider: &PonProvider<'_, S>, roots: &BTreeSet<String>) -> Option<Error> {
    provider
        .rejects
        .borrow()
        .iter()
        .find_map(|((name, _version), reasons)| {
            if !roots.contains(name) {
                return None;
            }
            reasons
                .iter()
                .find(|reason| reason.contains("C-ABI") || reason.contains("ob_refcnt") || reason.contains("CPython"))
                .map(|reason| {
                    Error::UnsupportedArtifact(format!(
                        "package `{name}` requires the CPython C-ABI (ob_refcnt): {reason}; this is a by-design limitation of pon"
                    ))
                })
        })
}

impl<'a, S: CandidateSource> DependencyProvider for PonProvider<'a, S> {
    type P = PonPackage;
    type V = Version;
    type VS = Ranges<Version>;
    type M = String;
    type Priority = (u32, Reverse<usize>);
    type Err = Error;

    fn prioritize(
        &self,
        package: &Self::P,
        range: &Self::VS,
        package_statistics: &PackageResolutionStatistics,
    ) -> Self::Priority {
        let version_count = self
            .versions_for_package(package)
            .map(|versions| versions.into_iter().filter(|version| range.contains(version)).count())
            .unwrap_or(0);
        if version_count == 0 {
            return (u32::MAX, Reverse(0));
        }
        (package_statistics.conflict_count(), Reverse(version_count))
    }

    fn choose_version(&self, package: &Self::P, range: &Self::VS) -> Result<Option<Self::V>, Self::Err> {
        if matches!(package, PonPackage::Root) {
            let version = root_version();
            return Ok(range.contains(&version).then_some(version));
        }

        let name = package.dist_name().expect("non-root package has a distribution name");
        let mut versions = self.versions_for_package(package)?;
        versions.sort();
        for version in versions.into_iter().rev() {
            if !range.contains(&version) {
                continue;
            }
            if self.pinned_candidate(name).is_some() {
                return Ok(Some(version));
            }
            let artifacts = self.source.artifacts(name, &version)?;
            let has_installable_wheel = artifacts.wheels.iter().any(|file| !file.kind.is_refused());
            let has_installable_sdist = source_distribution_allowed(&artifacts);
            if has_installable_wheel || has_installable_sdist {
                return Ok(Some(version));
            }
            self.record_rejects(name, &version, &artifacts);
        }
        Ok(None)
    }

    fn get_dependencies(
        &self,
        package: &Self::P,
        version: &Self::V,
    ) -> Result<Dependencies<Self::P, Self::VS, Self::M>, Self::Err> {
        match self.dependency_edges(package, version) {
            Ok(edges) => Ok(Dependencies::Available(edges)),
            Err(Error::UnsupportedArtifact(message)) => Ok(Dependencies::Unavailable(message)),
            Err(error) => Err(error),
        }
    }
}

pub fn resolve_root<S: CandidateSource>(provider: &PonProvider<'_, S>) -> Result<Resolution> {
    let selected = match pubgrub_resolve(provider, PonPackage::Root, root_version()) {
        Ok(selected) => selected,
        Err(PubGrubError::NoSolution(mut tree)) => {
            tree.collapse_no_versions();
            let packages = tree
                .packages()
                .into_iter()
                .filter_map(PonPackage::dist_name)
                .map(names::normalize)
                .collect::<BTreeSet<_>>();
            let mut report = DefaultStringReporter::report(&tree);
            let reject_lines = provider.reject_report_lines(&packages);
            if !reject_lines.is_empty() {
                if !report.is_empty() {
                    report.push('\n');
                }
                report.push_str(&reject_lines.join("\n"));
            }
            return Err(Error::InvalidRequirement(report));
        }
        Err(PubGrubError::ErrorRetrievingDependencies { source, .. })
        | Err(PubGrubError::ErrorChoosingVersion { source, .. })
        | Err(PubGrubError::ErrorInShouldCancel(source)) => return Err(source),
    };

    let mut dist_versions = BTreeMap::<String, Version>::new();
    let mut selected_packages = Vec::<(PonPackage, Version)>::new();
    for (package, version) in selected.iter() {
        selected_packages.push((package.clone(), version.clone()));
        if let PonPackage::Dist(name) = package {
            dist_versions.insert(name.clone(), version.clone());
        }
    }

    let dependency_graph = dependency_graph(provider, &selected_packages, &dist_versions)?;
    let ordered_names = dependency_order(&dependency_graph);
    let mut dists = Vec::new();
    for name in ordered_names {
        let Some(version) = dist_versions.get(&name) else {
            continue;
        };
        let (artifact, kind) = provider.selected_artifact(&name, version)?;
        let dependencies = dependency_graph
            .get(&name)
            .map(|deps| deps.iter().cloned().collect())
            .unwrap_or_default();
        dists.push(ResolvedDist {
            name,
            version: version.clone(),
            kind,
            artifact,
            dependencies,
            marker: None,
        });
    }
    Ok(Resolution { dists })
}

fn dependency_graph<S: CandidateSource>(
    provider: &PonProvider<'_, S>,
    selected_packages: &[(PonPackage, Version)],
    dist_versions: &BTreeMap<String, Version>,
) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let mut graph = dist_versions
        .keys()
        .map(|name| (name.clone(), BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();

    for (package, version) in selected_packages {
        let Some(owner) = package.dist_name().map(names::normalize) else {
            continue;
        };
        let edges = provider.dependency_edges(package, version)?;
        let deps = graph.entry(owner.clone()).or_default();
        for (dependency, _) in edges {
            if let Some(dep_name) = dependency.dist_name().map(names::normalize) {
                if dep_name != owner && dist_versions.contains_key(&dep_name) {
                    deps.insert(dep_name);
                }
            }
        }
    }
    Ok(graph)
}

fn dependency_order(graph: &BTreeMap<String, BTreeSet<String>>) -> Vec<String> {
    let mut ordered = Vec::new();
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for name in graph.keys() {
        visit_dependency(name, graph, &mut visiting, &mut visited, &mut ordered);
    }
    ordered
}

fn visit_dependency(
    name: &str,
    graph: &BTreeMap<String, BTreeSet<String>>,
    visiting: &mut BTreeSet<String>,
    visited: &mut BTreeSet<String>,
    ordered: &mut Vec<String>,
) {
    if visited.contains(name) {
        return;
    }
    if !visiting.insert(name.to_owned()) {
        return;
    }
    if let Some(dependencies) = graph.get(name) {
        for dependency in dependencies {
            visit_dependency(dependency, graph, visiting, visited, ordered);
        }
    }
    visiting.remove(name);
    visited.insert(name.to_owned());
    ordered.push(name.to_owned());
}

fn resolved_input_from_raw(raw: &str, marker_env: &MarkerEnvironment) -> Result<Option<ResolvedInput>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(Error::InvalidRequirement(raw.to_owned()));
    }

    match parse_requirement_input(raw)? {
        RequirementInput::Path { path, editable } => Ok(Some(ResolvedInput::Pinned {
            raw: raw.to_owned(),
            candidate: pin_local_path(&path, editable)?,
        })),
        RequirementInput::Url { url } => {
            let url = url.to_string();
            if is_git_direct_url(&url) {
                return Ok(Some(ResolvedInput::Pinned {
                    raw: raw.to_owned(),
                    candidate: pin_git_url(&url)?,
                }));
            }
            Err(unsupported_direct_url(&url))
        }
        RequirementInput::Pep508(requirement) => {
            if !requirement.evaluate_markers(marker_env, &[]) {
                return Ok(None);
            }
            if let Some(VersionOrUrl::Url(url)) = &requirement.version_or_url {
                let url = url.to_string();
                if is_git_direct_url(&url) {
                    let candidate = pin_git_url(&url)?;
                    validate_pinned_requirement_name(&candidate, &requirement)?;
                    return Ok(Some(ResolvedInput::Pinned {
                        raw: raw.to_owned(),
                        candidate,
                    }));
                }
                return Err(unsupported_direct_url(&url));
            }
            Ok(Some(ResolvedInput::Registry {
                raw: raw.to_owned(),
                requirement,
            }))
        }
    }
}

fn is_git_direct_url(url: &str) -> bool {
    url.trim_start().starts_with("git+")
}

fn unsupported_direct_url(url: &str) -> Error {
    Error::InvalidRequirement(format!("direct URL sources are not supported yet: {url}"))
}

fn validate_pinned_requirement_name(candidate: &PinnedCandidate, requirement: &Requirement) -> Result<()> {
    let requested_name = names::normalize(requirement.name.as_ref());
    if candidate.name == requested_name {
        return Ok(());
    }

    Err(Error::InvalidRequirement(format!(
        "direct URL requirement `{requirement}` resolved to package `{}`",
        candidate.name
    )))
}

fn vcs_cache_root() -> PathBuf {
    std::env::var_os("PON_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".pon"))
        .join("cache")
}

fn apply_legacy_version_specifier(input: &mut ResolvedInput, raw: &str, version_specifier: &str) -> Result<()> {
    let version_specifier = version_specifier.trim();
    if version_specifier.is_empty() {
        return Ok(());
    }

    let ResolvedInput::Registry { requirement, .. } = input else {
        return Ok(());
    };
    let raw = raw.trim();
    if names::validate(raw).is_ok()
        && names::normalize(raw) == requirement.name.as_ref()
        && requirement.version_or_url.is_none()
    {
        requirement.version_or_url = Some(VersionOrUrl::VersionSpecifier(parse_version_specifiers(
            version_specifier,
        )?));
    }
    Ok(())
}

impl ResolvedInput {
    fn dist_name(&self) -> &str {
        match self {
            Self::Registry { requirement, .. } => requirement.name.as_ref(),
            Self::Pinned { candidate, .. } => candidate.name.as_str(),
        }
    }
}

impl PinnedArtifact {
    fn to_resolved(&self, version: &Version, kind: &PackageKind) -> ResolvedArtifact {
        match self {
            Self::Dir { path, editable } => ResolvedArtifact::Dir {
                path: path.clone(),
                editable: *editable,
            },
            Self::Sdist { path } => ResolvedArtifact::Sdist(local_project_file(path, version.clone(), kind.clone())),
            Self::Vcs {
                url,
                requested_rev,
                commit,
                dir,
            } => ResolvedArtifact::Vcs {
                url: url.clone(),
                requested_rev: requested_rev.clone(),
                commit: commit.clone(),
                dir: dir.clone(),
            },
        }
    }
}

fn pin_local_path(path: &Path, editable: bool) -> Result<PinnedCandidate> {
    let (pyproject, artifact) = if path.is_dir() {
        let manifest_path = path.join("pyproject.toml");
        if !manifest_path.is_file() {
            return Err(Error::InvalidRequirement(format!(
                "unsupported local package source `{}`",
                path.display()
            )));
        }
        (PyProject::read(&manifest_path)?, PinnedArtifact::Dir {
            path: path.to_path_buf(),
            editable,
        })
    } else if path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.ends_with(".tar.gz")) {
        let (content, label) = read_tar_gz_pyproject(path)?;
        (PyProject::from_str(label, &content)?, PinnedArtifact::Sdist { path: path.to_path_buf() })
    } else if path.extension().and_then(|extension| extension.to_str()) == Some("zip") {
        let (content, label) = read_zip_pyproject(path)?;
        (PyProject::from_str(label, &content)?, PinnedArtifact::Sdist { path: path.to_path_buf() })
    } else {
        return Err(Error::InvalidRequirement(format!(
            "unsupported local package source `{}`",
            path.display()
        )));
    };

    let label = pyproject.path.display().to_string();
    let raw_name = pyproject
        .project_name()
        .ok_or_else(|| Error::InvalidRequirement(format!("{label} is missing [project].name")))?;
    names::validate(raw_name)?;
    let name = names::normalize(raw_name);
    let raw_version = pyproject
        .project_version()
        .ok_or_else(|| Error::InvalidRequirement(format!("{label} is missing [project].version")))?;
    let version = Version::from_str(raw_version).map_err(|_| {
        Error::InvalidRequirement(format!("{label} has invalid [project].version `{raw_version}`"))
    })?;
    let kind = if pyproject.tool_pon_native_import_name().is_some() {
        PackageKind::Native
    } else {
        PackageKind::Pure
    };
    let dependencies = pyproject
        .dependencies()
        .into_iter()
        .map(|raw| match parse_requirement_input(&raw)? {
            RequirementInput::Pep508(requirement) => match &requirement.version_or_url {
                Some(VersionOrUrl::Url(url)) if !is_git_direct_url(&url.to_string()) => {
                    Err(Error::InvalidRequirement(format!(
                        "direct URL sources are not supported yet: {}",
                        requirement
                    )))
                }
                _ => Ok(requirement),
            },
            RequirementInput::Path { .. } | RequirementInput::Url { .. } => Err(Error::InvalidRequirement(format!(
                "local package dependency `{raw}` is not a PEP 508 registry requirement"
            ))),
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(PinnedCandidate {
        name,
        version,
        kind,
        artifact,
        dependencies,
    })
}

fn pin_git_url(url: &str) -> Result<PinnedCandidate> {
    let reference = crate::vcs::parse_git_reference(url)?;
    let checkout = crate::vcs::fetch_git(&vcs_cache_root(), url, None)?;
    let mut candidate = pin_local_path(&checkout.dir, false)?;
    candidate.artifact = PinnedArtifact::Vcs {
        url: url.to_owned(),
        requested_rev: reference.requested_rev,
        commit: checkout.commit,
        dir: checkout.dir,
    };
    Ok(candidate)
}

fn core_metadata_from_pinned(candidate: &PinnedCandidate) -> CoreMetadata {
    CoreMetadata {
        metadata_version: "2.1".to_owned(),
        name: candidate.name.clone(),
        version: candidate.version.clone(),
        requires_dist: candidate.dependencies.clone(),
        requires_python: None,
        provides_extra: Vec::new(),
        summary: None,
        license: None,
        author: None,
        author_email: None,
        home_page: None,
        project_urls: Vec::new(),
        classifiers: Vec::new(),
        dynamic: Vec::new(),
    }
}

fn local_project_file(path: &Path, version: Version, kind: PackageKind) -> ProjectFile {
    let filename = path.display().to_string();
    ProjectFile {
        url: format!("file://{}", path.display()),
        filename,
        version,
        kind,
        hashes: BTreeMap::new(),
        requires_python: None,
        requires_python_invalid: false,
        yanked: None,
        dist_info_metadata: None,
    }
}

fn read_tar_gz_pyproject(path: &Path) -> Result<(String, String)> {
    let file = File::open(path).map_err(|_| {
        Error::InvalidRequirement(format!(
            "unsupported local package source `{}`",
            path.display()
        ))
    })?;
    let decoder = GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let entries = archive.entries().map_err(|error| {
        Error::InvalidRequirement(format!("failed to read sdist `{}`: {error}", path.display()))
    })?;
    for entry in entries {
        let mut entry = entry.map_err(|error| {
            Error::InvalidRequirement(format!("failed to read sdist `{}`: {error}", path.display()))
        })?;
        let entry_path = entry
            .path()
            .map_err(|error| {
                Error::InvalidRequirement(format!(
                    "failed to read sdist `{}` member path: {error}",
                    path.display()
                ))
            })?
            .into_owned();
        if entry_path.file_name().and_then(|name| name.to_str()) == Some("pyproject.toml") {
            let mut content = String::new();
            entry.read_to_string(&mut content).map_err(|error| {
                Error::InvalidRequirement(format!(
                    "failed to read pyproject.toml from sdist `{}`: {error}",
                    path.display()
                ))
            })?;
            let label = format!("{}:{}", path.display(), entry_path.display());
            return Ok((content, label));
        }
    }
    Err(Error::InvalidRequirement(format!(
        "sdist `{}` is missing pyproject.toml",
        path.display()
    )))
}

fn read_zip_pyproject(path: &Path) -> Result<(String, String)> {
    let label = path.display().to_string();
    let file = File::open(path).map_err(|_| {
        Error::InvalidRequirement(format!(
            "unsupported local package source `{}`",
            path.display()
        ))
    })?;
    let mut archive = ZipArchive::new(file).map_err(|error| zip_error(&label, error))?;
    for index in 0..archive.len() {
        let mut member = archive.by_index(index).map_err(|error| zip_error(&label, error))?;
        let member_name = member.name().to_owned();
        if member_name.rsplit('/').next() == Some("pyproject.toml") {
            let mut content = String::new();
            member.read_to_string(&mut content)?;
            return Ok((content, format!("{}:{}", path.display(), member_name)));
        }
    }
    Err(Error::InvalidRequirement(format!(
        "sdist `{}` is missing pyproject.toml",
        path.display()
    )))
}

fn zip_error(label: &str, error: ZipError) -> Error {
    Error::UnsupportedArtifact(format!("failed to read wheel `{label}`: {error}"))
}

fn parse_version_specifiers(raw: &str) -> Result<VersionSpecifiers> {
    let trimmed = raw.trim();
    if trimmed == "*" {
        return Ok(VersionSpecifiers::default());
    }
    VersionSpecifiers::from_str(trimmed).map_err(|_| Error::InvalidSpecifier(raw.to_owned()))
}

fn is_exact_equal_specifier(specifiers: &VersionSpecifiers) -> bool {
    specifiers.len() == 1 && specifiers[0].operator() == &Operator::Equal
}

fn root_version() -> Version {
    Version::from_str("0").expect("root version literal is valid PEP 440")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::str::FromStr;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::index::{DistInfoMetadata, SimpleJsonIndex};

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
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .join("fixtures")
            .join("fastjson-pon");
        let record = provider.resolve_input(fixture.to_str().expect("fixture path"), "").expect("record");

        assert_eq!(record, PackageRecord {
            name: "fastjson-pon".to_owned(),
            version: "0.1.0".to_owned(),
            kind: PackageKind::Native,
        });
    }

    #[test]
    fn refuses_numpy_cabi_only_candidate() {
        let provider = ResolveProvider::default();
        let error = provider.resolve_input("numpy", "").expect_err("c-abi should be refused");
        let message = error.to_string();

        assert!(message.contains("refusing numpy"));
        assert!(message.contains("requires the CPython C-ABI"));
        assert!(message.contains("by-design limitation of pon"));
    }

    #[test]
    fn resolves_three_level_chain_from_sidecar_metadata() {
        let index = cached_chain_index();
        let provider = ResolveProvider::with_marker_env(index, pon_marker_env());

        let resolved = provider.resolve_requirements(["pkg-a"].iter().copied()).expect("resolve");

        assert_eq!(
            resolved.iter().map(|package| package.record.name.as_str()).collect::<Vec<_>>(),
            ["pkg-c", "pkg-b", "pkg-a"]
        );
        assert_eq!(
            resolved.iter().map(|package| package.record.version.as_str()).collect::<Vec<_>>(),
            ["1.0.0", "1.0.0", "1.0.0"]
        );
    }

    #[test]
    fn pubgrub_backtracks_to_older_compatible_version() {
        let mut source = StaticSource::default();
        source.add("a", "1.0", ["c>=2"]);
        source.add("a", "2.0", ["c<2"]);
        source.add("c", "1.0", []);
        source.add("c", "2.0", []);
        let provider = PonProvider::from_requirements(&source, ["a", "c>=2"]).expect("provider");

        let resolution = resolve_root(&provider).expect("resolution");
        let versions = resolution
            .dists
            .iter()
            .map(|dist| (dist.name.as_str(), dist.version.to_string()))
            .collect::<BTreeMap<_, _>>();

        assert_eq!(versions.get("a").map(String::as_str), Some("1.0"));
        assert_eq!(versions.get("c").map(String::as_str), Some("2.0"));
    }

    #[test]
    fn pubgrub_conflict_report_names_root_and_transitive_constraints() {
        let mut source = StaticSource::default();
        source.add("a", "1.0", ["c<2"]);
        source.add("c", "1.0", []);
        source.add("c", "2.0", []);
        let provider = PonProvider::from_requirements(&source, ["a", "c>=2"]).expect("provider");

        let error = resolve_root(&provider).expect_err("conflict should be reported");
        let message = error.to_string();

        assert!(message.contains("a"));
        assert!(message.contains("c"));
        assert!(message.contains("1.0"), "{message}");
        assert!(message.contains("2.0"), "{message}");
    }

    fn cached_chain_index() -> SimpleJsonIndex {
        let root = temp_project("chain-index");
        let cache = root.join("cache");
        let index = SimpleJsonIndex::with_cache_dir("https://fixtures.example/simple/", &cache);
        for (name, body) in [
            ("pkg-a", include_str!("../index/fixtures/pkg-a-pep691.json").to_owned()),
            ("pkg-b", include_str!("../index/fixtures/pkg-b-pep691.json").to_owned()),
            (
                "pkg-c",
                include_str!("../index/fixtures/pkg-c-pep691.json").replace(
                    "\"requires-python\": \">=3.8\",",
                    "\"requires-python\": \">=3.8\", \"dist-info-metadata\": { \"sha256\": \"3333333333333333333333333333333333333333333333333333333333333333\" },",
                ),
            ),
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
            (
                "https://files.example/pkg_c-1.0.0-py3-none-any.whl.metadata",
                "Metadata-Version: 2.3\nName: pkg-c\nVersion: 1.0.0\n",
            ),
        ] {
            let path = index.cache_path_for_url(url);
            fs::create_dir_all(path.parent().expect("parent")).expect("metadata parent");
            fs::write(path, metadata).expect("metadata cache");
        }
        index
    }

    #[derive(Default)]
    struct StaticSource {
        candidates: BTreeMap<String, BTreeMap<Version, StaticCandidate>>,
    }

    struct StaticCandidate {
        metadata: CoreMetadata,
        artifact: ProjectFile,
    }

    impl StaticSource {
        fn add<const N: usize>(&mut self, name: &str, version: &str, requirements: [&str; N]) {
            let version = Version::from_str(version).expect("version");
            let normalized = names::normalize(name);
            let metadata = CoreMetadata {
                metadata_version: "2.3".to_owned(),
                name: normalized.clone(),
                version: version.clone(),
                requires_dist: requirements
                    .into_iter()
                    .map(|requirement| Requirement::from_str(requirement).expect("requirement"))
                    .collect(),
                requires_python: None,
                provides_extra: Vec::new(),
                summary: None,
                license: None,
                author: None,
                author_email: None,
                home_page: None,
                project_urls: Vec::new(),
                classifiers: Vec::new(),
                dynamic: Vec::new(),
            };
            let artifact = project_file(&normalized, &version);
            self.candidates
                .entry(normalized)
                .or_default()
                .insert(version, StaticCandidate { metadata, artifact });
        }
    }

    impl CandidateSource for StaticSource {
        fn available_versions(&self, name: &str, _include_yanked: bool) -> Result<Vec<Version>> {
            Ok(self
                .candidates
                .get(&names::normalize(name))
                .map(|versions| versions.keys().cloned().collect())
                .unwrap_or_default())
        }

        fn artifacts(&self, name: &str, version: &Version) -> Result<ArtifactSet> {
            Ok(self
                .candidates
                .get(&names::normalize(name))
                .and_then(|versions| versions.get(version))
                .map(|candidate| ArtifactSet {
                    wheels: vec![candidate.artifact.clone()],
                    sdist: None,
                })
                .unwrap_or_default())
        }

        fn metadata(&self, name: &str, version: &Version) -> Result<CoreMetadata> {
            self.candidates
                .get(&names::normalize(name))
                .and_then(|versions| versions.get(version))
                .map(|candidate| candidate.metadata.clone())
                .ok_or_else(|| Error::InvalidRequirement(format!("unknown package `{name}`")))
        }
    }

    fn project_file(name: &str, version: &Version) -> ProjectFile {
        ProjectFile {
            filename: format!("{}-{}-py3-none-any.whl", name.replace('-', "_"), version),
            url: format!("https://files.example/{}-{}-py3-none-any.whl", name.replace('-', "_"), version),
            version: version.clone(),
            kind: PackageKind::Pure,
            hashes: BTreeMap::new(),
            requires_python: None,
            requires_python_invalid: false,
            yanked: None,
            dist_info_metadata: Some(DistInfoMetadata {
                hashes: BTreeMap::new(),
            }),
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
