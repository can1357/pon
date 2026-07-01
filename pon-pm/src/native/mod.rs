use std::fs;
use std::path::{Path, PathBuf};

use crate::env::EnvLayout;
use crate::error::{Error, Result};
use crate::install::{
    InstallReport, InstalledPackageRecord, ResolvedRecord, upsert_installed_package, validate_registry_field,
};
use crate::names;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NativeRegistry {
    pub modules: Vec<NativeModuleRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeModuleRecord {
    pub package_name: String,
    pub import_name: String,
    pub version: String,
    pub manifest_path: PathBuf,
}

struct NativeManifest {
    package_name: String,
    version: String,
    import_name: String,
}

pub fn install_local_package(env: &EnvLayout, resolved_record: &ResolvedRecord, path: &Path) -> Result<InstallReport> {
    let manifest_path = path.join("pyproject.toml");
    let manifest = read_native_manifest(&manifest_path)?;
    let normalized_manifest_name = names::normalize(&manifest.package_name);
    let normalized_record_name = resolved_record.normalized_name();
    if normalized_manifest_name != normalized_record_name {
        return Err(Error::UnsupportedArtifact(format!(
            "local package `{}` does not match resolved package `{}`",
            normalized_manifest_name, normalized_record_name
        )));
    }
    if manifest.version != resolved_record.version {
        return Err(Error::UnsupportedArtifact(format!(
            "local package `{}` version `{}` does not match resolved version `{}`",
            manifest.package_name, manifest.version, resolved_record.version
        )));
    }

    env.create_dirs()?;
    materialize_native_marker(env, &manifest)?;
    let mut registry = read_registry(env)?;
    registry.upsert(NativeModuleRecord {
        package_name: normalized_manifest_name.clone(),
        import_name: manifest.import_name.clone(),
        version: manifest.version.clone(),
        manifest_path,
    });
    write_registry(env, &registry)?;
    upsert_installed_package(
        env,
        InstalledPackageRecord {
            name: normalized_manifest_name.clone(),
            version: manifest.version.clone(),
            artifact_kind: "pon-native".to_owned(),
            import_names: vec![manifest.import_name.clone()],
            record_path: None,
        },
    )?;
    Ok(InstallReport {
        package_name: normalized_manifest_name,
        version: manifest.version,
        artifact_kind: "pon-native".to_owned(),
        import_names: vec![manifest.import_name],
    })
}

pub fn read_registry(env: &EnvLayout) -> Result<NativeRegistry> {
    let content = match fs::read_to_string(&env.native_registry_path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(NativeRegistry::default()),
        Err(error) => return Err(error.into()),
    };
    let mut registry = NativeRegistry::default();
    for (line_index, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 4 {
            return Err(Error::UnsupportedArtifact(format!(
                "invalid native registry line {} in {}",
                line_index + 1,
                env.native_registry_path.display()
            )));
        }
        registry.modules.push(NativeModuleRecord {
            package_name: fields[0].to_owned(),
            import_name: fields[1].to_owned(),
            version: fields[2].to_owned(),
            manifest_path: PathBuf::from(fields[3]),
        });
    }
    Ok(registry)
}

pub fn write_registry(env: &EnvLayout, registry: &NativeRegistry) -> Result<()> {
    if let Some(parent) = env.native_registry_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut content = String::new();
    for module in &registry.modules {
        validate_registry_field(&module.package_name, "native package name")?;
        validate_registry_field(&module.import_name, "native import name")?;
        validate_registry_field(&module.version, "native version")?;
        let manifest_path = module.manifest_path.to_string_lossy();
        validate_registry_field(&manifest_path, "native manifest path")?;
        content.push_str(&module.package_name);
        content.push('\t');
        content.push_str(&module.import_name);
        content.push('\t');
        content.push_str(&module.version);
        content.push('\t');
        content.push_str(&manifest_path);
        content.push('\n');
    }
    fs::write(&env.native_registry_path, content)?;
    Ok(())
}

impl NativeRegistry {
    pub fn upsert(&mut self, record: NativeModuleRecord) {
        if let Some(existing) = self
            .modules
            .iter_mut()
            .find(|candidate| candidate.import_name == record.import_name)
        {
            *existing = record;
        } else {
            self.modules.push(record);
        }
        self.modules.sort_by(|left, right| left.import_name.cmp(&right.import_name));
    }
}

fn read_native_manifest(path: &Path) -> Result<NativeManifest> {
    let content = fs::read_to_string(path)?;
    let package_name = toml_string(&content, "project", "name")
        .ok_or_else(|| Error::UnsupportedArtifact(format!("{} is missing [project].name", path.display())))?;
    let version = toml_string(&content, "project", "version")
        .ok_or_else(|| Error::UnsupportedArtifact(format!("{} is missing [project].version", path.display())))?;
    let import_name = toml_string(&content, "tool.pon.native", "import-name").ok_or_else(|| {
        Error::UnsupportedArtifact(format!(
            "{} is missing [tool.pon.native].import-name",
            path.display()
        ))
    })?;
    if !is_import_name(&import_name) {
        return Err(Error::UnsupportedArtifact(format!(
            "native import name `{import_name}` must be a Python identifier"
        )));
    }
    Ok(NativeManifest {
        package_name,
        version,
        import_name,
    })
}

fn materialize_native_marker(env: &EnvLayout, manifest: &NativeManifest) -> Result<()> {
    let marker = format!(
        "VERSION = {:?}\n__version__ = {:?}\n__pon_native_package__ = {:?}\n",
        manifest.version, manifest.version, manifest.package_name
    );
    fs::write(env.site_packages.join(format!("{}.py", manifest.import_name)), marker)?;
    Ok(())
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
        if candidate_key.trim() != key {
            continue;
        }
        return parse_basic_toml_string(value.trim());
    }
    None
}

fn parse_basic_toml_string(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    if bytes.len() < 2 || bytes.first().copied() != Some(b'"') || bytes.last().copied() != Some(b'"') {
        return None;
    }
    Some(value[1..value.len() - 1].to_owned())
}

fn is_import_name(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic()) && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::install::{ResolvedRecord, read_installed_packages};

    #[test]
    fn native_registry_round_trips_records() {
        let layout = EnvLayout::new(temp_project("registry"));
        let registry = NativeRegistry {
            modules: vec![NativeModuleRecord {
                package_name: "fastjson-pon".to_owned(),
                import_name: "fastjson".to_owned(),
                version: "0.1.0".to_owned(),
                manifest_path: PathBuf::from("fixtures/fastjson-pon/pyproject.toml"),
            }],
        };

        write_registry(&layout, &registry).expect("write");
        let round_trip = read_registry(&layout).expect("read");

        assert_eq!(round_trip, registry);
    }

    #[test]
    fn installs_fastjson_pon_fixture_metadata() {
        let layout = EnvLayout::new(temp_project("fastjson"));
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/fastjson-pon");
        let record = ResolvedRecord::local_path("fastjson-pon", "0.1.0", &fixture);

        let report = install_local_package(&layout, &record, &fixture).expect("install");

        assert_eq!(report.import_names, vec!["fastjson"]);
        assert_eq!(report.version, "0.1.0");
        let marker = fs::read_to_string(layout.site_packages.join("fastjson.py")).expect("marker");
        assert!(marker.contains("VERSION = \"0.1.0\""));
        let registry = read_registry(&layout).expect("registry");
        assert_eq!(registry.modules.len(), 1);
        assert_eq!(registry.modules[0].import_name, "fastjson");
        assert_eq!(registry.modules[0].version, "0.1.0");
        let installed = read_installed_packages(&layout).expect("installed registry");
        assert_eq!(installed[0].artifact_kind, "pon-native");
        assert_eq!(installed[0].import_names, vec!["fastjson"]);
    }

    #[test]
    fn rejects_local_fixture_version_mismatch() {
        let layout = EnvLayout::new(temp_project("fastjson-version"));
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/fastjson-pon");
        let record = ResolvedRecord::local_path("fastjson-pon", "9.9.9", &fixture);

        let error = install_local_package(&layout, &record, &fixture).expect_err("version mismatch");

        assert!(error.to_string().contains("does not match resolved version"));
    }

    fn temp_project(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("pon-pm-native-{label}-{unique}"))
    }
}
