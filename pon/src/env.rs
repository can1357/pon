use std::path::{Path, PathBuf};

use crate::error::Result;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvLayout {
    pub project_root: PathBuf,
    pub pon_dir: PathBuf,
    pub packages_dir: PathBuf,
    pub site_packages: PathBuf,
    pub registry_path: PathBuf,
    pub native_dir: PathBuf,
    pub native_registry_path: PathBuf,
    pub scripts_dir: PathBuf,
    pub manifest_path: PathBuf,
}

impl EnvLayout {
    #[must_use]
    pub fn new(project_root: impl AsRef<Path>) -> Self {
        let project_root = project_root.as_ref().to_path_buf();
        let pon_dir = project_root.join(".pon");
        let packages_dir = pon_dir.join("packages");
        let site_packages = packages_dir.join("site-packages");
        let registry_path = packages_dir.join("installed.tsv");
        let native_dir = pon_dir.join("native");
        let native_registry_path = native_dir.join("registry.tsv");
        let scripts_dir = pon_dir.join(if cfg!(windows) { "Scripts" } else { "bin" });
        let manifest_path = project_root.join("pyproject.toml");
        Self {
            project_root,
            pon_dir,
            packages_dir,
            site_packages,
            registry_path,
            native_dir,
            native_registry_path,
            scripts_dir,
            manifest_path,
        }
    }

    #[must_use]
    pub fn import_paths(&self) -> Vec<PathBuf> {
        vec![self.site_packages.clone(), self.project_root.clone()]
    }

    #[must_use]
    pub fn import_path_string(&self) -> String {
        let separator = if cfg!(windows) { ";" } else { ":" };
        self.import_paths()
            .iter()
            .map(|path| path.to_string_lossy())
            .collect::<Vec<_>>()
            .join(separator)
    }

    pub fn create_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.site_packages)?;
        std::fs::create_dir_all(&self.packages_dir)?;
        std::fs::create_dir_all(&self.native_dir)?;
        std::fs::create_dir_all(&self.scripts_dir)?;
        Ok(())
    }
}

#[must_use]
pub fn default_layout() -> EnvLayout {
    EnvLayout::new(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_dot_pon_layout() {
        let layout = EnvLayout::new("/tmp/project");
        assert_eq!(layout.pon_dir, PathBuf::from("/tmp/project/.pon"));
        assert_eq!(layout.packages_dir, PathBuf::from("/tmp/project/.pon/packages"));
        assert_eq!(
            layout.site_packages,
            PathBuf::from("/tmp/project/.pon/packages/site-packages")
        );
        assert_eq!(layout.manifest_path, PathBuf::from("/tmp/project/pyproject.toml"));
    }

    #[test]
    fn import_path_prefers_managed_packages_before_project_root() {
        let layout = EnvLayout::new("project");
        assert_eq!(
            layout.import_paths(),
            vec![
                PathBuf::from("project/.pon/packages/site-packages"),
                PathBuf::from("project")
            ]
        );
    }
}
