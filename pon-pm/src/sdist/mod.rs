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
        env,
        normalized_name: &normalized_name,
        version: &resolved_record.version,
        filename,
    })?;
    crate::wheel::install_wheel(env, &ResolvedRecord::wheel(&resolved_record.name, &resolved_record.version, &build_artifact.wheel_filename), &build_artifact.wheel_filename)
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
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::install::read_installed_packages;

    #[test]
    fn builds_flit_fixture_sdist_and_installs_wheel_contents() {
        let layout = EnvLayout::new(temp_project("flit-fixture-sdist"));
        let filename = fixture_sdist_path("pon-flit-fixture-0.1.0.tar.gz");
        let filename = filename.display().to_string();
        let record = ResolvedRecord::sdist("pon-flit-fixture", "0.1.0", &filename);

        let report = install_sdist(&layout, &record, &filename).expect("install sdist");

        assert_eq!(report.package_name, "pon-flit-fixture");
        assert_eq!(report.version, "0.1.0");
        assert_eq!(report.artifact_kind, "wheel");
        assert_eq!(report.import_names, vec!["pon_flit_fixture"]);

        let package_init = layout.site_packages.join("pon_flit_fixture/__init__.py");
        assert_eq!(
            fs::read_to_string(&package_init).expect("installed package"),
            "__version__ = \"0.1.0\"\n"
        );

        let record_path = layout.site_packages.join("pon_flit_fixture-0.1.0.dist-info/RECORD");
        let record_text = fs::read_to_string(&record_path).expect("installed RECORD");
        assert!(record_text.contains("pon_flit_fixture/__init__.py,sha256="));
        assert!(record_text.contains("pon_flit_fixture-0.1.0.dist-info/RECORD,,"));

        let registry = read_installed_packages(&layout).expect("registry");
        assert_eq!(registry.len(), 1);
        assert_eq!(registry[0].name, "pon-flit-fixture");
        assert_eq!(registry[0].version, "0.1.0");
        assert_eq!(registry[0].artifact_kind, "wheel");
        assert_eq!(registry[0].import_names, vec!["pon_flit_fixture"]);
        assert_eq!(
            registry[0].record_path,
            Some(std::path::PathBuf::from("pon_flit_fixture-0.1.0.dist-info/RECORD"))
        );
    }

    #[test]
    fn rejects_setuptools_backend_sdist_with_backend_name() {
        let layout = EnvLayout::new(temp_project("setuptools-sdist"));
        let filename = fixture_sdist_path("pon-setuptools-fixture-0.1.0.tar.gz");
        let filename = filename.display().to_string();
        let record = ResolvedRecord::sdist("pon-setuptools-fixture", "0.1.0", &filename);

        let error = install_sdist(&layout, &record, &filename).expect_err("unsupported backend");

        let Error::UnsupportedArtifact(message) = error else {
            panic!("expected UnsupportedArtifact");
        };
        assert!(message.contains("setuptools.build_meta"));
    }

    fn fixture_sdist_path(filename: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join("sdists")
            .join(filename)
    }

    fn temp_project(label: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("pon-pm-sdist-{label}-{unique}"))
    }
}
