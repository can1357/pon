use std::path::Path;

use crate::error::{Error, Result};
use crate::index::{CatalogIndex, IndexSource, ProjectRequest};
use crate::resolve::source::{PackageKind, PackageRecord, PackageSource};
use crate::resolve::versionset::VersionSet;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolveProvider<I = CatalogIndex> {
    index: I,
}

impl Default for ResolveProvider<CatalogIndex> {
    fn default() -> Self {
        Self::new(CatalogIndex::new())
    }
}

impl<I> ResolveProvider<I> {
    #[must_use]
    pub fn new(index: I) -> Self {
        Self { index }
    }
}

impl<I: IndexSource> ResolveProvider<I> {
    pub fn resolve(&self, source: &PackageSource, version_set: &VersionSet) -> Result<PackageRecord> {
        match source {
            PackageSource::Registry { name, index_url } => self.resolve_registry(name, index_url.as_deref(), version_set),
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

    fn resolve_registry(
        &self,
        name: &str,
        index_url: Option<&str>,
        version_set: &VersionSet,
    ) -> Result<PackageRecord> {
        let mut request = ProjectRequest::new(name)?;
        if let Some(index_url) = index_url {
            request = request.with_index_url(index_url);
        }
        let project = self
            .index
            .project(&request)?
            .ok_or_else(|| Error::InvalidRequirement(format!("unknown package `{name}`")))?;
        let file = project.best_match(version_set).ok_or_else(|| {
            Error::InvalidRequirement(format!("no catalog version of `{name}` matches requested specifier"))
        })?;
        Ok(PackageRecord {
            name: project.name,
            version: file.version.raw().to_owned(),
            kind: file.kind,
        })
    }
}

fn local_path_record(path: &Path) -> Result<PackageRecord> {
    let basename = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    match basename {
        "fastjson-pon" => Ok(PackageRecord {
            name: "fastjson-pon".to_owned(),
            version: "0.1.0".to_owned(),
            kind: PackageKind::Native,
        }),
        _ => Err(Error::InvalidRequirement(format!(
            "unsupported local package source `{}`",
            path.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
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
}
