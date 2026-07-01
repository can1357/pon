pub mod compat;
pub mod filename;
pub(crate) mod record;

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use zip::ZipArchive;
use zip::result::ZipError;

use crate::env::EnvLayout;
use crate::error::{Error, Result};
use crate::install::{InstallReport, InstalledPackageRecord, ResolvedRecord, upsert_installed_package};

use self::compat::{WheelCompatibility, classify_archive_member, classify_root_is_purelib, classify_tags, default_supported_tags};
use self::filename::WheelFilename;
use self::record::{RecordEntry, parse_record};

struct CatalogPackage {
    normalized_name: &'static str,
    import_name: &'static str,
    module_body_prefix: &'static str,
}

struct WheelInspection {
    record_path: PathBuf,
    record_text: String,
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
    let source_path = wheel_source_path(filename)?;
    let mut archive = open_archive(&source_path, filename)?;
    let inspection = inspect_archive(filename, &mut archive)?;
    let record_entries = parse_record(&inspection.record_text, &inspection.record_path.display().to_string())?;
    let package = catalog_package(&normalized_record_name)?;

    env.create_dirs()?;
    extract_archive(env, filename, &mut archive, &record_entries)?;
    upsert_installed_package(
        env,
        InstalledPackageRecord {
            name: normalized_record_name.clone(),
            version: resolved_record.version.clone(),
            artifact_kind: "wheel".to_owned(),
            import_names: vec![package.import_name.to_owned()],
            record_path: Some(inspection.record_path),
        },
    )?;
    Ok(InstallReport {
        package_name: normalized_record_name,
        version: resolved_record.version.clone(),
        artifact_kind: "wheel".to_owned(),
        import_names: vec![package.import_name.to_owned()],
    })
}

pub fn install_catalog_package(
    env: &EnvLayout,
    normalized_name: &str,
    version: &str,
    artifact_kind: &str,
) -> Result<InstallReport> {
    let package = catalog_package(normalized_name)?;

    env.create_dirs()?;
    materialize_import_marker(env, package, version)?;
    upsert_installed_package(
        env,
        InstalledPackageRecord {
            name: normalized_name.to_owned(),
            version: version.to_owned(),
            artifact_kind: artifact_kind.to_owned(),
            import_names: vec![package.import_name.to_owned()],
            record_path: None,
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
    match classify_tags(&wheel.tags(), &default_supported_tags()) {
        WheelCompatibility::PurePython => Ok(wheel),
        WheelCompatibility::CAbiRefused { reason } => Err(cabi_refused(filename, &reason)),
    }
}

fn catalog_package(normalized_name: &str) -> Result<&'static CatalogPackage> {
    CATALOG_PACKAGES
        .iter()
        .find(|package| package.normalized_name == normalized_name)
        .ok_or_else(|| {
            Error::UnsupportedArtifact(format!(
                "package `{normalized_name}` is not in the deterministic Pon package catalog"
            ))
        })
}

fn wheel_source_path(filename: &str) -> Result<PathBuf> {
    let path = Path::new(filename);
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    let basename = path.file_name().and_then(|name| name.to_str()).unwrap_or(filename);
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("wheels")
        .join(basename);
    if fixture_path.is_file() {
        Ok(fixture_path)
    } else {
        Err(Error::UnsupportedArtifact(format!(
            "wheel `{filename}` is not available in the bundled Pon wheel fixtures"
        )))
    }
}

fn open_archive(path: &Path, label: &str) -> Result<ZipArchive<File>> {
    let file = File::open(path)?;
    ZipArchive::new(file).map_err(|error| zip_error(label, error))
}

fn inspect_archive(filename: &str, archive: &mut ZipArchive<File>) -> Result<WheelInspection> {
    let mut wheel_metadata = None;
    let mut record_path = None;
    let mut record_text = None;

    for index in 0..archive.len() {
        let mut member = archive.by_index(index).map_err(|error| zip_error(filename, error))?;
        let member_name = member.name().to_owned();
        match classify_archive_member(&member_name) {
            WheelCompatibility::PurePython => {}
            WheelCompatibility::CAbiRefused { reason } => return Err(cabi_refused(filename, &reason)),
        }
        if member_name.ends_with(".dist-info/WHEEL") {
            let mut metadata = String::new();
            member.read_to_string(&mut metadata)?;
            wheel_metadata = Some(metadata);
        } else if member_name.ends_with(".dist-info/RECORD") {
            let mut text = String::new();
            member.read_to_string(&mut text)?;
            record_path = Some(safe_site_relative_path(&member_name)?);
            record_text = Some(text);
        }
    }

    let metadata = wheel_metadata.ok_or_else(|| {
        Error::UnsupportedArtifact(format!("wheel `{filename}` does not contain a .dist-info/WHEEL member"))
    })?;
    match classify_root_is_purelib(&metadata) {
        WheelCompatibility::PurePython => {}
        WheelCompatibility::CAbiRefused { reason } => return Err(cabi_refused(filename, &reason)),
    }

    Ok(WheelInspection {
        record_path: record_path.ok_or_else(|| {
            Error::UnsupportedArtifact(format!("wheel `{filename}` does not contain a .dist-info/RECORD member"))
        })?,
        record_text: record_text.expect("record path and text are set together"),
    })
}

fn extract_archive(
    env: &EnvLayout,
    filename: &str,
    archive: &mut ZipArchive<File>,
    record_entries: &[RecordEntry],
) -> Result<()> {
    let records = record_entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect::<BTreeMap<_, _>>();

    for index in 0..archive.len() {
        let mut member = archive.by_index(index).map_err(|error| zip_error(filename, error))?;
        let member_name = member.name().to_owned();
        let relative_path = safe_site_relative_path(&member_name)?;
        let destination = env.site_packages.join(&relative_path);

        if member.is_dir() {
            fs::create_dir_all(&destination)?;
            continue;
        }

        let Some(record_entry) = records.get(member_name.as_str()).copied() else {
            return Err(Error::UnsupportedArtifact(format!(
                "wheel `{filename}` member `{member_name}` is missing from RECORD"
            )));
        };

        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = File::create(&destination)?;
        let mut hasher = Sha256::new();
        let copied = copy_and_hash(&mut member, &mut out, &mut hasher)?;
        if let Err(error) = verify_record_entry(filename, record_entry, copied, hasher.finalize()) {
            let _ = fs::remove_file(&destination);
            return Err(error);
        }
    }
    Ok(())
}

fn copy_and_hash(reader: &mut impl Read, writer: &mut impl Write, hasher: &mut Sha256) -> Result<u64> {
    let mut buffer = [0_u8; 16 * 1024];
    let mut copied = 0_u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(copied);
        }
        writer.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        copied += read as u64;
    }
}

fn verify_record_entry(
    filename: &str,
    record_entry: &RecordEntry,
    copied: u64,
    digest: impl AsRef<[u8]>,
) -> Result<()> {
    if let Some(size) = record_entry.size {
        if size != copied {
            return Err(Error::UnsupportedArtifact(format!(
                "wheel `{filename}` member `{}` failed RECORD size verification: expected {size}, got {copied}",
                record_entry.path
            )));
        }
    }
    let Some(hash) = &record_entry.hash else {
        return Ok(());
    };
    let Some((algorithm, expected)) = hash.split_once('=') else {
        return Err(Error::UnsupportedArtifact(format!(
            "wheel `{filename}` member `{}` has invalid RECORD hash `{hash}`",
            record_entry.path
        )));
    };
    if algorithm != "sha256" {
        return Err(Error::UnsupportedArtifact(format!(
            "wheel `{filename}` member `{}` uses unsupported RECORD hash algorithm `{algorithm}`",
            record_entry.path
        )));
    }
    let actual = URL_SAFE_NO_PAD.encode(digest.as_ref());
    if actual != expected {
        return Err(Error::UnsupportedArtifact(format!(
            "wheel `{filename}` member `{}` failed RECORD hash verification",
            record_entry.path
        )));
    }
    Ok(())
}

fn safe_site_relative_path(path: &str) -> Result<PathBuf> {
    if path.is_empty() || path.contains('\\') {
        return Err(Error::UnsupportedArtifact(format!("unsafe wheel member path `{path}`")));
    }
    let path = Path::new(path);
    if path.is_absolute() {
        return Err(Error::UnsupportedArtifact(format!("unsafe wheel member path `{}`", path.display())));
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(Error::UnsupportedArtifact(format!("unsafe wheel member path `{}`", path.display())));
            }
        }
    }
    if out.as_os_str().is_empty() {
        return Err(Error::UnsupportedArtifact(format!("unsafe wheel member path `{}`", path.display())));
    }
    Ok(out)
}

fn cabi_refused(filename: &str, reason: &str) -> Error {
    Error::UnsupportedArtifact(format!(
        "wheel `{filename}` requires the CPython C-ABI (ob_refcnt): {reason}; this is a by-design limitation of pon"
    ))
}

fn zip_error(filename: &str, error: ZipError) -> Error {
    Error::UnsupportedArtifact(format!("invalid wheel archive `{filename}`: {error}"))
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
    use crate::install::{read_installed_packages, remove_installed_package};

    #[test]
    fn installs_idna_pure_wheel_by_extracting_fixture_and_registry_record() {
        let layout = EnvLayout::new(temp_project("idna-wheel"));
        let record = ResolvedRecord::wheel("idna", "3.10", "idna-3.10-py3-none-any.whl");

        let report = install_wheel(&layout, &record, "idna-3.10-py3-none-any.whl").expect("install");

        assert_eq!(report.import_names, vec!["idna"]);
        assert!(layout.site_packages.join("idna").join("__init__.py").is_file());
        assert!(layout.site_packages.join("idna-3.10.dist-info").join("RECORD").is_file());
        let registry = read_installed_packages(&layout).expect("registry");
        assert_eq!(registry.len(), 1);
        assert_eq!(registry[0].name, "idna");
        assert_eq!(registry[0].import_names, vec!["idna"]);
        assert_eq!(
            registry[0].record_path.as_deref(),
            Some(Path::new("idna-3.10.dist-info/RECORD"))
        );
    }

    #[test]
    fn installs_flit_core_pure_wheel_by_extracting_fixture() {
        let layout = EnvLayout::new(temp_project("flit-core-wheel"));
        let record = ResolvedRecord::wheel("flit-core", "3.12.0", "flit_core-3.12.0-py3-none-any.whl");

        install_wheel(&layout, &record, "flit_core-3.12.0-py3-none-any.whl").expect("install");

        let marker = fs::read_to_string(layout.site_packages.join("flit_core").join("__init__.py")).expect("marker");
        assert!(marker.contains("__version__ = \"3.12.0\""));
    }

    #[test]
    fn remove_round_trip_deletes_record_files_and_empty_parents() {
        let layout = EnvLayout::new(temp_project("remove-wheel"));
        let record = ResolvedRecord::wheel("idna", "3.10", "idna-3.10-py3-none-any.whl");
        install_wheel(&layout, &record, "idna-3.10-py3-none-any.whl").expect("install");

        let removed = remove_installed_package(&layout, "IDNA").expect("remove");

        assert!(removed.is_some());
        assert!(!layout.site_packages.join("idna").exists());
        assert!(!layout.site_packages.join("idna-3.10.dist-info").exists());
        assert!(read_installed_packages(&layout).expect("registry").is_empty());
        let remaining = fs::read_dir(&layout.site_packages)
            .expect("site-packages")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("entries");
        assert!(remaining.is_empty());
    }

    #[test]
    fn refuses_c_abi_platform_wheel_with_explicit_reason() {
        let error = validate_compatible_wheel("numpy-2.0.0-cp312-cp312-macosx_14_0_arm64.whl")
            .expect_err("platform wheel should fail");
        let message = error.to_string();
        assert!(message.contains("requires the CPython C-ABI"));
        assert!(message.contains("ob_refcnt"));
    }

    #[test]
    fn refuses_mistagged_wheel_with_native_member() {
        let root = temp_project("native-member");
        fs::create_dir_all(&root).expect("root");
        let wheel_path = root.join("demo-1.0-py3-none-any.whl");
        write_test_wheel(
            &wheel_path,
            [
                ("demo/__init__.py", b"__version__ = '1.0'\n".as_slice()),
                ("demo/_native.so", b"not really a shared library".as_slice()),
                (
                    "demo-1.0.dist-info/WHEEL",
                    b"Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\n".as_slice(),
                ),
                ("demo-1.0.dist-info/METADATA", b"Name: demo\nVersion: 1.0\n".as_slice()),
            ],
        );
        let layout = EnvLayout::new(root.join("project"));
        let record = ResolvedRecord::wheel("demo", "1.0", wheel_path.display().to_string());

        let error = install_wheel(&layout, &record, &wheel_path.display().to_string()).expect_err("native member");

        let message = error.to_string();
        assert!(message.contains("ob_refcnt"));
        assert!(message.contains("native extension member"));
    }

    fn write_test_wheel<const N: usize>(path: &Path, members: [(&str, &[u8]); N]) {
        use zip::CompressionMethod;
        use zip::write::SimpleFileOptions;

        let file = File::create(path).expect("wheel");
        let mut zip = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let mut record_rows = Vec::new();
        for (name, bytes) in members {
            zip.start_file(name, options).expect("member");
            zip.write_all(bytes).expect("body");
            let digest = URL_SAFE_NO_PAD.encode(Sha256::digest(bytes));
            record_rows.push(format!("{name},sha256={digest},{}", bytes.len()));
        }
        record_rows.push("demo-1.0.dist-info/RECORD,,".to_owned());
        zip.start_file("demo-1.0.dist-info/RECORD", options).expect("record");
        zip.write_all(record_rows.join("\n").as_bytes()).expect("record body");
        zip.write_all(b"\n").expect("record newline");
        zip.finish().expect("finish");
    }

    fn temp_project(label: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("pon-pm-wheel-{label}-{unique}"))
    }
}
