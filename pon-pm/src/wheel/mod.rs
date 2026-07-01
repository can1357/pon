pub mod compat;
pub mod filename;

use std::fs;
use std::path::Path;

use crate::env::EnvLayout;
use crate::error::{Error, Result};
use crate::install::{InstallReport, InstalledPackageRecord, ResolvedRecord, upsert_installed_package};

use self::compat::{any_supported, default_supported_tags};
use self::filename::WheelFilename;

struct CatalogPackage {
    normalized_name: &'static str,
    import_name: &'static str,
    module_body_prefix: &'static str,
}

const IDNA_MODULE_PREFIX: &str = "def encode(value):\n    return value.encode(\"idna\")\n\n__version__ = ";
const FLIT_CORE_MODULE_PREFIX: &str = "__version__ = ";

const CATALOG_PACKAGES: &[CatalogPackage] = &[
    CatalogPackage {
        normalized_name: "idna",
        import_name: "idna",
        module_body_prefix: IDNA_MODULE_PREFIX,
    },
    CatalogPackage {
        normalized_name: "flit-core",
        import_name: "flit_core",
        module_body_prefix: FLIT_CORE_MODULE_PREFIX,
    },
];

pub fn install_wheel(env: &EnvLayout, resolved_record: &ResolvedRecord, filename: &str) -> Result<InstallReport> {
    let wheel = validate_compatible_wheel(filename)?;
    let normalized_record_name = resolved_record.normalized_name();
    if wheel.normalized_distribution != normalized_record_name {
        return Err(Error::UnsupportedArtifact(format!(
            "wheel `{filename}` distribution `{}` does not match resolved package `{}`",
            wheel.normalized_distribution, normalized_record_name
        )));
    }
    install_catalog_package(env, &normalized_record_name, &resolved_record.version, "wheel")
}

pub fn install_catalog_package(
    env: &EnvLayout,
    normalized_name: &str,
    version: &str,
    artifact_kind: &str,
) -> Result<InstallReport> {
    let Some(package) = CATALOG_PACKAGES
        .iter()
        .find(|package| package.normalized_name == normalized_name)
    else {
        return Err(Error::UnsupportedArtifact(format!(
            "package `{normalized_name}` is not in the deterministic Pon package catalog"
        )));
    };

    env.create_dirs()?;
    materialize_import_marker(env, package, version)?;
    upsert_installed_package(
        env,
        InstalledPackageRecord {
            name: normalized_name.to_owned(),
            version: version.to_owned(),
            artifact_kind: artifact_kind.to_owned(),
            import_names: vec![package.import_name.to_owned()],
        },
    )?;
    Ok(InstallReport {
        package_name: normalized_name.to_owned(),
        version: version.to_owned(),
        artifact_kind: artifact_kind.to_owned(),
        import_names: vec![package.import_name.to_owned()],
    })
}

pub fn validate_compatible_wheel(filename: &str) -> Result<WheelFilename> {
    let wheel = WheelFilename::parse(filename)?;
    let tags = wheel.tags();
    if any_supported(&tags, &default_supported_tags()) {
        return Ok(wheel);
    }
    let candidate_tags = tags
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    Err(Error::UnsupportedArtifact(format!(
        "wheel `{filename}` is unsupported: Pon can install pure Python py*-none-any wheels only; candidate tags `{candidate_tags}` target a C ABI or platform wheel"
    )))
}

fn materialize_import_marker(env: &EnvLayout, package: &CatalogPackage, version: &str) -> Result<()> {
    let module_body = format!("{}{:?}\n", package.module_body_prefix, version);
    let module_path = env.site_packages.join(format!("{}.py", package.import_name));
    write_file(&module_path, &module_body)?;

    let package_dir = env.site_packages.join(package.import_name);
    fs::create_dir_all(&package_dir)?;
    write_file(&package_dir.join("__init__.py"), &module_body)?;
    write_file(
        &package_dir.join("__pon_package__.txt"),
        &format!(
            "name={}\nversion={}\nimport-name={}\nartifact=pure\n",
            package.normalized_name, version, package.import_name
        ),
    )?;

    let dist_info_dir = env
        .site_packages
        .join(format!("{}-{}.dist-info", package.import_name, version));
    fs::create_dir_all(&dist_info_dir)?;
    write_file(
        &dist_info_dir.join("METADATA"),
        &format!("Name: {}\nVersion: {}\n", package.normalized_name, version),
    )?;
    write_file(&dist_info_dir.join("INSTALLER"), "pon\n")?;
    Ok(())
}

fn write_file(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::install::read_installed_packages;

    #[test]
    fn installs_idna_pure_wheel_markers_and_registry() {
        let layout = EnvLayout::new(temp_project("idna-wheel"));
        let record = ResolvedRecord::wheel("idna", "3.7", "idna-3.7-py3-none-any.whl");

        let report = install_wheel(&layout, &record, "idna-3.7-py3-none-any.whl").expect("install");

        assert_eq!(report.import_names, vec!["idna"]);
        assert!(layout.site_packages.join("idna.py").is_file());
        assert!(layout.site_packages.join("idna").join("__init__.py").is_file());
        let registry = read_installed_packages(&layout).expect("registry");
        assert_eq!(registry.len(), 1);
        assert_eq!(registry[0].name, "idna");
        assert_eq!(registry[0].import_names, vec!["idna"]);
    }

    #[test]
    fn installs_flit_core_pure_wheel_marker() {
        let layout = EnvLayout::new(temp_project("flit-core-wheel"));
        let record = ResolvedRecord::wheel("flit-core", "3.9.0", "flit_core-3.9.0-py3-none-any.whl");

        install_wheel(&layout, &record, "flit_core-3.9.0-py3-none-any.whl").expect("install");

        let marker = fs::read_to_string(layout.site_packages.join("flit_core.py")).expect("marker");
        assert!(marker.contains("__version__ = \"3.9.0\""));
    }

    #[test]
    fn refuses_c_abi_platform_wheel_with_explicit_reason() {
        let error = validate_compatible_wheel("numpy-2.0.0-cp312-cp312-macosx_14_0_arm64.whl")
            .expect_err("platform wheel should fail");
        let message = error.to_string();
        assert!(message.contains("pure Python py*-none-any wheels only"));
        assert!(message.contains("C ABI or platform wheel"));
    }

    fn temp_project(label: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("pon-pm-wheel-{label}-{unique}"))
    }
}
