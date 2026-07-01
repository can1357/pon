use crate::error::{Error, Result};
use crate::names;
use crate::resolve::source::PackageKind;
use crate::resolve::versionset::{Version, VersionSet};

pub const DEFAULT_INDEX_URL: &str = "https://pypi.org/simple/";
pub const NO_OB_REFCNT_C_ABI_REFUSAL: &str =
    "refusing numpy: no-ob_refcnt C-ABI support is available in Pon";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectRequest {
    pub name: String,
    pub index_url: String,
}

impl ProjectRequest {
    pub fn new(name: impl AsRef<str>) -> Result<Self> {
        let name = name.as_ref();
        names::validate(name)?;
        Ok(Self {
            name: names::normalize(name),
            index_url: DEFAULT_INDEX_URL.to_owned(),
        })
    }

    #[must_use]
    pub fn with_index_url(mut self, index_url: impl Into<String>) -> Self {
        self.index_url = index_url.into();
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectPage {
    pub meta_api_version: String,
    pub name: String,
    pub files: Vec<ProjectFile>,
}

impl ProjectPage {
    #[must_use]
    pub fn versions(&self) -> Vec<Version> {
        let mut versions = self.files.iter().map(|file| file.version.clone()).collect::<Vec<_>>();
        versions.sort();
        versions.dedup();
        versions
    }

    #[must_use]
    pub fn best_match(&self, version_set: &VersionSet) -> Option<ProjectFile> {
        self.files
            .iter()
            .filter(|file| version_set.contains(&file.version))
            .max_by(|left, right| left.version.cmp(&right.version).then_with(|| left.filename.cmp(&right.filename)))
            .cloned()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectFile {
    pub filename: String,
    pub url: String,
    pub version: Version,
    pub kind: PackageKind,
}

pub trait IndexSource {
    fn project(&self, request: &ProjectRequest) -> Result<Option<ProjectPage>>;
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CatalogIndex;

impl CatalogIndex {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    pub fn lookup(&self, name: impl AsRef<str>) -> Result<Option<ProjectPage>> {
        let request = ProjectRequest::new(name)?;
        self.project(&request)
    }

    pub fn versions(&self, name: impl AsRef<str>) -> Result<Vec<Version>> {
        Ok(self.lookup(name)?.map_or_else(Vec::new, |project| project.versions()))
    }
}

impl IndexSource for CatalogIndex {
    fn project(&self, request: &ProjectRequest) -> Result<Option<ProjectPage>> {
        Ok(match request.name.as_str() {
            "idna" => Some(project(
                "idna",
                [
                    file("idna-3.9-py3-none-any.whl", "3.9", PackageKind::Pure)?,
                    file("idna-3.10-py3-none-any.whl", "3.10", PackageKind::Pure)?,
                ],
            )),
            "flit-core" => Some(project(
                "flit-core",
                [file("flit_core-3.12.0-py3-none-any.whl", "3.12.0", PackageKind::Pure)?],
            )),
            "numpy" => Some(project(
                "numpy",
                [file(
                    "numpy-2.3.1-cp314-cp314-macosx_14_0_arm64.whl",
                    "2.3.1",
                    PackageKind::CAbiRefused {
                        reason: NO_OB_REFCNT_C_ABI_REFUSAL.to_owned(),
                    },
                )?],
            )),
            _ => None,
        })
    }
}

fn project<const N: usize>(name: &str, files: [ProjectFile; N]) -> ProjectPage {
    ProjectPage {
        meta_api_version: "1.0".to_owned(),
        name: name.to_owned(),
        files: Vec::from(files),
    }
}

fn file(filename: &str, version: &str, kind: PackageKind) -> Result<ProjectFile> {
    Ok(ProjectFile {
        filename: filename.to_owned(),
        url: format!("{DEFAULT_INDEX_URL}{filename}"),
        version: Version::parse(version).map_err(|_| Error::InvalidRequirement(filename.to_owned()))?,
        kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_lookup_normalizes_names_and_returns_pep_691_shape() {
        let index = CatalogIndex::new();
        let project = index.lookup("Flit_Core").expect("lookup").expect("project");

        assert_eq!(project.meta_api_version, "1.0");
        assert_eq!(project.name, "flit-core");
        assert_eq!(project.files[0].filename, "flit_core-3.12.0-py3-none-any.whl");
    }

    #[test]
    fn version_set_selects_highest_matching_catalog_file() {
        let index = CatalogIndex::new();
        let project = index.lookup("idna").expect("lookup").expect("project");
        let version_set = VersionSet::parse("<3.10").expect("version set");
        let best = project.best_match(&version_set).expect("best match");

        assert_eq!(project.versions().iter().map(Version::raw).collect::<Vec<_>>(), ["3.9", "3.10"]);
        assert_eq!(best.version.raw(), "3.9");
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
