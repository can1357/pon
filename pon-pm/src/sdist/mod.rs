pub mod build;

use crate::env::EnvLayout;
use crate::error::{Error, Result};
use crate::install::{InstallReport, ResolvedRecord};
use crate::names;

use self::build::{BuildRequest, CatalogSdistBuilder, SdistBuilder};

pub fn install_sdist(env: &EnvLayout, resolved_record: &ResolvedRecord, filename: &str) -> Result<InstallReport> {
    install_sdist_with_builder(env, resolved_record, filename, &CatalogSdistBuilder)
}

pub fn install_sdist_with_builder(
    env: &EnvLayout,
    resolved_record: &ResolvedRecord,
    filename: &str,
    builder: &impl SdistBuilder,
) -> Result<InstallReport> {
    let normalized_name = normalized_name_from_sdist(filename)?;
    let resolved_name = resolved_record.normalized_name();
    if normalized_name != resolved_name {
        return Err(Error::UnsupportedArtifact(format!(
            "sdist `{filename}` distribution `{normalized_name}` does not match resolved package `{resolved_name}`"
        )));
    }
    let build_artifact = builder.build(&BuildRequest {
        normalized_name: &normalized_name,
        version: &resolved_record.version,
        filename,
    })?;
    crate::wheel::validate_compatible_wheel(&build_artifact.wheel_filename)?;
    crate::wheel::install_catalog_package(env, &normalized_name, &resolved_record.version, "sdist")
}

fn normalized_name_from_sdist(filename: &str) -> Result<String> {
    let basename = std::path::Path::new(filename)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(filename);
    let stem = basename
        .strip_suffix(".tar.gz")
        .or_else(|| basename.strip_suffix(".zip"))
        .ok_or_else(|| Error::UnsupportedArtifact(format!("sdist `{filename}` must end in .tar.gz or .zip")))?;
    let Some((name, version)) = stem.rsplit_once('-') else {
        return Err(Error::UnsupportedArtifact(format!(
            "sdist `{filename}` does not contain a distribution and version"
        )));
    };
    if name.is_empty() || version.is_empty() {
        return Err(Error::UnsupportedArtifact(format!(
            "sdist `{filename}` does not contain a distribution and version"
        )));
    }
    Ok(names::normalize(name))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::install::read_installed_packages;
    use crate::sdist::build::BuildArtifact;

    struct RecordingBuilder {
        called: Cell<bool>,
    }

    impl SdistBuilder for RecordingBuilder {
        fn build(&self, request: &BuildRequest<'_>) -> Result<BuildArtifact> {
            self.called.set(true);
            assert_eq!(request.normalized_name, "flit-core");
            assert_eq!(request.filename, "flit-core-3.9.0.tar.gz");
            Ok(BuildArtifact {
                wheel_filename: format!("flit_core-{}-py3-none-any.whl", request.version),
            })
        }
    }

    #[test]
    fn installs_flit_core_sdist_through_build_seam() {
        let layout = EnvLayout::new(temp_project("flit-core-sdist"));
        let record = ResolvedRecord::sdist("flit-core", "3.9.0", "flit-core-3.9.0.tar.gz");
        let builder = RecordingBuilder {
            called: Cell::new(false),
        };

        let report = install_sdist_with_builder(&layout, &record, "flit-core-3.9.0.tar.gz", &builder).expect("install");

        assert!(builder.called.get());
        assert_eq!(report.artifact_kind, "sdist");
        assert_eq!(report.import_names, vec!["flit_core"]);
        assert!(layout.site_packages.join("flit_core.py").is_file());
        let marker = fs::read_to_string(layout.site_packages.join("flit_core.py")).expect("marker");
        assert!(marker.contains("__version__ = \"3.9.0\""));
        let registry = read_installed_packages(&layout).expect("registry");
        assert_eq!(registry[0].artifact_kind, "sdist");
    }

    #[test]
    fn rejects_unknown_sdist_catalog_package() {
        let layout = EnvLayout::new(temp_project("unknown-sdist"));
        let record = ResolvedRecord::sdist("demo", "1.0", "demo-1.0.tar.gz");

        let error = install_sdist(&layout, &record, "demo-1.0.tar.gz").expect_err("unknown");

        assert!(error.to_string().contains("deterministic sdist catalog"));
    }

    fn temp_project(label: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("pon-pm-sdist-{label}-{unique}"))
    }
}
