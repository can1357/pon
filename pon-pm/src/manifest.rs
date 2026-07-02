use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::names;

#[path = "pyproject.rs"]
pub mod pyproject;
pub use self::pyproject::{BuildSystem, PonSource, PyProject};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Requirement {
    raw: String,
    normalized_name: String,
}

impl Requirement {
    pub fn parse(raw: impl AsRef<str>) -> Result<Self> {
        let raw = raw.as_ref().trim();
        if raw.is_empty() {
            return Err(Error::InvalidRequirement(raw.to_owned()));
        }

        if raw.starts_with('.') || raw.starts_with('/') || raw.contains(std::path::MAIN_SEPARATOR) {
            let normalized_name = path_requirement_name(raw).ok_or_else(|| Error::InvalidRequirement(raw.to_owned()))?;
            return Ok(Self {
                raw: raw.to_owned(),
                normalized_name,
            });
        }

        let name_end = raw
            .char_indices()
            .find_map(|(index, ch)| {
                if matches!(ch, '[' | '<' | '>' | '=' | '!' | '~' | ';' | '@') || ch.is_whitespace() {
                    Some(index)
                } else {
                    None
                }
            })
            .unwrap_or(raw.len());
        let name = &raw[..name_end];
        names::validate(name).map_err(|_| Error::InvalidRequirement(raw.to_owned()))?;

        Ok(Self {
            raw: raw.to_owned(),
            normalized_name: names::normalize(name),
        })
    }

    pub fn for_resolved_package(raw: impl AsRef<str>, normalized_name: impl AsRef<str>) -> Result<Self> {
        let raw = raw.as_ref().trim();
        if raw.is_empty() {
            return Err(Error::InvalidRequirement(raw.to_owned()));
        }
        let normalized_name = normalized_name.as_ref();
        names::validate(normalized_name)?;
        Ok(Self {
            raw: raw.to_owned(),
            normalized_name: names::normalize(normalized_name),
        })
    }

    #[must_use]
    pub fn raw(&self) -> &str {
        &self.raw
    }

    #[must_use]
    pub fn normalized_name(&self) -> &str {
        &self.normalized_name
    }
}

fn path_requirement_name(raw: &str) -> Option<String> {
    let basename = Path::new(raw).file_name()?.to_str()?;
    let distribution = basename
        .strip_suffix(".tar.gz")
        .and_then(|stem| stem.rsplit_once('-').map(|(name, _version)| name))
        .unwrap_or(basename);
    names::validate(distribution).ok()?;
    Some(names::normalize(distribution))
}

pub struct ProjectManifest {
    pub path: PathBuf,
    pyproject: PyProject,
    dependencies: BTreeMap<String, Requirement>,
}

impl ProjectManifest {
    #[must_use]
    pub fn empty(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        Self {
            path: path.clone(),
            pyproject: PyProject::empty(path),
            dependencies: BTreeMap::new(),
        }
    }

    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_pyproject(PyProject::read(path.as_ref())?)
    }

    pub fn from_str(path: impl Into<PathBuf>, content: &str) -> Result<Self> {
        Self::from_pyproject(PyProject::from_str(path, content)?)
    }

    fn from_pyproject(pyproject: PyProject) -> Result<Self> {
        let mut dependencies = BTreeMap::new();
        for raw in pyproject.dependencies() {
            let requirement = Requirement::parse(&raw)?;
            dependencies.insert(requirement.normalized_name.clone(), requirement);
        }
        Ok(Self {
            path: pyproject.path.clone(),
            pyproject,
            dependencies,
        })
    }

    #[must_use]
    pub fn dependencies(&self) -> Vec<&Requirement> {
        self.dependencies.values().collect()
    }

    pub fn add(&mut self, requirement: Requirement) -> bool {
        self.dependencies
            .insert(requirement.normalized_name.clone(), requirement)
            .is_none()
    }

    pub fn remove(&mut self, name: &str) -> Result<bool> {
        names::validate(name)?;
        Ok(self.dependencies.remove(&names::normalize(name)).is_some())
    }

    pub fn write(&self) -> Result<()> {
        let mut pyproject = self.pyproject.clone();
        pyproject.set_dependency_strings(self.dependencies.values().map(Requirement::raw));
        pyproject.write()
    }
}

pub fn add_dependency(path: impl AsRef<Path>, requirement: impl AsRef<str>) -> Result<bool> {
    let mut pyproject = PyProject::read(path.as_ref())?;
    let changed = pyproject.add_dependency(requirement.as_ref())?;
    pyproject.write()?;
    Ok(changed)
}

pub fn remove_dependency(path: impl AsRef<Path>, name: impl AsRef<str>) -> Result<bool> {
    let mut pyproject = PyProject::read(path.as_ref())?;
    let changed = pyproject.remove_dependency(name.as_ref())?;
    pyproject.write()?;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_path(name: &str) -> PathBuf {
        let unique = format!(
            "pon-pm-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        );
        std::env::temp_dir().join(unique).join("pyproject.toml")
    }

    #[test]
    fn reads_project_dependencies() {
        let content = r#"[project]
name = "demo"
dependencies = [
    "Requests>=2",
    'friendly_bard',
]
"#;
        let manifest = ProjectManifest::from_str("pyproject.toml", content).expect("manifest");
        let deps = manifest
            .dependencies()
            .iter()
            .map(|req| (req.normalized_name(), req.raw()))
            .collect::<Vec<_>>();
        assert_eq!(deps, vec![("friendly-bard", "friendly_bard"), ("requests", "Requests>=2")]);
    }

    #[test]
    fn add_and_remove_dependency_rewrites_project_block() {
        let path = temp_path("rewrite");
        fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        fs::write(&path, "[build-system]\nrequires = []\n\n[project]\nname = \"demo\"\n").expect("write");

        assert!(add_dependency(&path, "Requests>=2").expect("add"));
        assert!(add_dependency(&path, "friendly_bard").expect("add second"));
        let content = fs::read_to_string(&path).expect("content");
        assert!(content.contains("[build-system]\nrequires = []"));
        assert!(content.contains("name = \"demo\""));
        assert!(content.contains("\"friendly_bard\""));
        assert!(content.contains("\"Requests>=2\""));

        assert!(remove_dependency(&path, "requests").expect("remove"));
        let manifest = ProjectManifest::read(&path).expect("read");
        assert_eq!(manifest.dependencies()[0].normalized_name(), "friendly-bard");
    }

    #[test]
    fn replacing_same_normalized_name_is_not_additive() {
        let mut manifest = ProjectManifest::empty("pyproject.toml");
        assert!(manifest.add(Requirement::parse("Requests>=2").expect("req")));
        assert!(!manifest.add(Requirement::parse("requests>=3").expect("req")));
        assert_eq!(manifest.dependencies()[0].raw(), "requests>=3");
    }
}
