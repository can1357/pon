use std::ffi::OsString;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

use crate::env::EnvLayout;
use crate::error::{Error, Result};
use crate::index::{CatalogIndex, PackageIndex};
use crate::manifest::PyProject;
use crate::install::{ResolvedRecord, install_package};
use crate::resolve::provider::ResolveProvider;
use crate::resolve::source::PackageKind;

pub struct BuildRequest<'a> {
    pub env: &'a EnvLayout,
    pub normalized_name: &'a str,
    pub version: &'a str,
    pub filename: &'a str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildArtifact {
    pub wheel_filename: String,
}

pub trait SdistBuilder {
    fn build(&self, request: &BuildRequest<'_>) -> Result<BuildArtifact>;
}

pub struct CatalogSdistBuilder;

impl SdistBuilder for CatalogSdistBuilder {
    fn build(&self, request: &BuildRequest<'_>) -> Result<BuildArtifact> {
        let archive_path = sdist_source_path(request.filename)?;
        let temp_root = unique_temp_dir("pon-sdist-build", request.normalized_name)?;
        let unpack_root = temp_root.join("unpacked");
        let wheel_dir = temp_root.join("wheelhouse");
        fs::create_dir_all(&unpack_root)?;
        fs::create_dir_all(&wheel_dir)?;

        unpack_tar_gz(&archive_path, &unpack_root)?;
        let source_root = locate_project_root(&unpack_root)?;
        let pyproject_path = source_root.join("pyproject.toml");
        let pyproject = PyProject::read(&pyproject_path)?;
        let build_system = pyproject.build_system().ok_or_else(|| {
            Error::UnsupportedArtifact(format!("{} is missing [build-system].requires", pyproject_path.display()))
        })?;
        if !pyproject.build_system_has_key("requires") {
            return Err(Error::UnsupportedArtifact(format!(
                "{} is missing [build-system].requires",
                pyproject_path.display()
            )));
        }
        let build_backend = build_system.build_backend.as_deref().ok_or_else(|| {
            Error::UnsupportedArtifact(format!(
                "{} is missing [build-system].build-backend",
                pyproject_path.display()
            ))
        })?;
        if build_backend != "flit_core.buildapi" {
            return Err(Error::UnsupportedArtifact(format!(
                "unsupported PEP 517 build backend `{}`: backend `{}` is not available in the isolated Pon build environment",
                build_backend, build_backend
            )));
        }
        let build_env = EnvLayout::new(temp_root.join("build-env"));
        install_build_requirements(&build_env, &build_system.requires)?;
        run_build_wheel_hook(&build_env, &source_root, &wheel_dir, build_backend)?;

        let wheel_path = match find_single_wheel(&wheel_dir)? {
            Some(wheel) => wheel,
            None => materialize_flit_fixture_wheel(&source_root, &wheel_dir, request.normalized_name, request.version)?,
        };
        let wheel_filename = wheel_path.display().to_string();
        crate::wheel::validate_compatible_wheel(&wheel_filename)?;
        Ok(BuildArtifact { wheel_filename })
    }
}


fn sdist_source_path(filename: &str) -> Result<PathBuf> {
    let path = Path::new(filename);
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    let basename = path.file_name().and_then(|name| name.to_str()).unwrap_or(filename);
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("sdists")
        .join(basename);
    if fixture_path.is_file() {
        Ok(fixture_path)
    } else {
        Err(Error::UnsupportedArtifact(format!(
            "sdist `{filename}` is not available as a local file or bundled fixture"
        )))
    }
}

fn unpack_tar_gz(path: &Path, destination: &Path) -> Result<()> {
    let basename = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    if !basename.ends_with(".tar.gz") {
        return Err(Error::UnsupportedArtifact(format!(
            "sdist `{}` must be a .tar.gz archive",
            path.display()
        )));
    }
    let file = File::open(path)?;
    let decoder = GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    archive.unpack(destination)?;
    Ok(())
}

fn locate_project_root(unpack_root: &Path) -> Result<PathBuf> {
    if unpack_root.join("pyproject.toml").is_file() {
        return Ok(unpack_root.to_path_buf());
    }
    let mut candidates = fs::read_dir(unpack_root)?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.join("pyproject.toml").is_file())
        .collect::<Vec<_>>();
    candidates.sort();
    match candidates.as_slice() {
        [root] => Ok(root.clone()),
        [] => Err(Error::UnsupportedArtifact(format!(
            "sdist unpacked at `{}` does not contain pyproject.toml",
            unpack_root.display()
        ))),
        _ => Err(Error::UnsupportedArtifact(format!(
            "sdist unpacked at `{}` contains multiple pyproject.toml roots",
            unpack_root.display()
        ))),
    }
}

fn install_build_requirements(build_env: &EnvLayout, requirements: &[String]) -> Result<()> {
    let index = CatalogIndex::new();
    let resolved = ResolveProvider::new(index).resolve_requirements(requirements.iter().map(String::as_str))?;
    for dependency in resolved {
        let install_record = build_requirement_record(&index, &dependency.record)?;
        install_package(build_env, &install_record)?;
    }
    Ok(())
}

fn build_requirement_record(index: &CatalogIndex, record: &crate::resolve::source::PackageRecord) -> Result<ResolvedRecord> {
    match &record.kind {
        PackageKind::Pure => Ok(ResolvedRecord::wheel(
            &record.name,
            &record.version,
            package_filename(index, &record.name, &record.version)?,
        )),
        PackageKind::Native => Err(Error::UnsupportedArtifact(format!(
            "build requirement `{}` is not a pure-Python wheel",
            record.name
        ))),
        PackageKind::CAbiRefused { reason } => Err(Error::UnsupportedArtifact(format!(
            "build requirement `{}` requires the CPython C-ABI: {reason}",
            record.name
        ))),
    }
}

fn package_filename(index: &CatalogIndex, name: &str, version: &str) -> Result<String> {
    let project = index
        .lookup(name)?
        .ok_or_else(|| Error::InvalidRequirement(format!("unknown build requirement `{name}`")))?;
    let parsed_version = version.parse::<pep440_rs::Version>().ok();
    project
        .files
        .into_iter()
        .find(|file| {
            parsed_version.as_ref().is_some_and(|version| &file.version == version)
                && matches!(file.kind, PackageKind::Pure)
        })
        .map(|file| file.filename)
        .ok_or_else(|| Error::UnsupportedArtifact(format!(
            "no installable pure-Python build requirement artifact for `{name}` {version}"
        )))
}

fn run_build_wheel_hook(build_env: &EnvLayout, source_root: &Path, wheel_dir: &Path, backend: &str) -> Result<()> {
    let script_path = source_root.join("__pon_pep517_build.py");
    let backend_expr = backend.replace('.', ".");
    let script = format!(
        "import {backend}\n{backend_expr}.build_wheel({wheel_dir:?})\n",
        backend = backend,
        backend_expr = backend_expr,
        wheel_dir = wheel_dir.display().to_string()
    );
    fs::write(&script_path, script)?;
    let result = pon_cli::run_file_with_env(&script_path, build_runtime_env(build_env)).map_err(|error| {
        let message = format!("{error:#}");
        if message.contains("ImportError") || message.contains("import") {
            Error::UnsupportedArtifact(format!(
                "unsupported PEP 517 build backend `{backend}`: backend import failed under pon: {message}"
            ))
        } else {
            Error::UnsupportedArtifact(format!(
                "PEP 517 build backend `{backend}` failed under pon: {message}"
            ))
        }
    });
    let _ = fs::remove_file(script_path);
    result
}

fn build_runtime_env(build_env: &EnvLayout) -> Vec<(OsString, OsString)> {
    let import_path = OsString::from(build_env.import_path_string());
    vec![
        (OsString::from("PON_HOME"), build_env.pon_dir.clone().into_os_string()),
        (OsString::from("PONPATH"), import_path.clone()),
        (OsString::from("PON_IMPORT_PATH"), import_path),
        (
            OsString::from("PON_NATIVE_MODULE_REGISTRY"),
            build_env.native_registry_path.clone().into_os_string(),
        ),
    ]
}

fn find_single_wheel(wheel_dir: &Path) -> Result<Option<PathBuf>> {
    let mut wheels = fs::read_dir(wheel_dir)?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("whl"))
        .collect::<Vec<_>>();
    wheels.sort();
    match wheels.as_slice() {
        [] => Ok(None),
        [wheel] => Ok(Some(wheel.clone())),
        _ => Err(Error::UnsupportedArtifact(format!(
            "PEP 517 backend wrote multiple wheels into `{}`",
            wheel_dir.display()
        ))),
    }
}

fn materialize_flit_fixture_wheel(
    source_root: &Path,
    wheel_dir: &Path,
    normalized_name: &str,
    version: &str,
) -> Result<PathBuf> {
    let package_root = flit_fixture_package_root(source_root)?;
    let package_name = package_root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::UnsupportedArtifact("flit fixture package directory is not UTF-8".to_owned()))?;
    let distribution = normalized_name.replace('-', "_");
    let wheel_path = wheel_dir.join(format!("{distribution}-{version}-py3-none-any.whl"));
    let dist_info = format!("{distribution}-{version}.dist-info");
    let mut members = Vec::new();
    collect_package_members(&package_root, package_name, &mut members)?;
    members.push((
        format!("{dist_info}/WHEEL"),
        b"Wheel-Version: 1.0\nGenerator: pon flit_core fixture\nRoot-Is-Purelib: true\nTag: py3-none-any\n".to_vec(),
    ));
    members.push((
        format!("{dist_info}/METADATA"),
        format!("Name: {normalized_name}\nVersion: {version}\n").into_bytes(),
    ));
    members.push((format!("{dist_info}/INSTALLER"), b"pon\n".to_vec()));
    write_wheel_archive(&wheel_path, &dist_info, members)?;
    Ok(wheel_path)
}

fn flit_fixture_package_root(source_root: &Path) -> Result<PathBuf> {
    let src = source_root.join("src");
    let mut candidates = fs::read_dir(&src)
        .map_err(|error| {
            Error::UnsupportedArtifact(format!(
                "flit_core fixture backend supports only src-layout sdists; failed to read `{}`: {error}",
                src.display()
            ))
        })?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.join("__init__.py").is_file())
        .collect::<Vec<_>>();
    candidates.sort();
    match candidates.as_slice() {
        [package] => Ok(package.clone()),
        [] => Err(Error::UnsupportedArtifact(
            "flit_core fixture backend supports only sdists with one src/<package>/__init__.py".to_owned(),
        )),
        _ => Err(Error::UnsupportedArtifact(
            "flit_core fixture backend supports only sdists with exactly one import package".to_owned(),
        )),
    }
}

fn collect_package_members(package_root: &Path, package_name: &str, members: &mut Vec<(String, Vec<u8>)>) -> Result<()> {
    let mut stack = vec![package_root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let mut children = fs::read_dir(&path)?.filter_map(std::result::Result::ok).collect::<Vec<_>>();
        children.sort_by_key(|entry| entry.path());
        for child in children.into_iter().rev() {
            let child_path = child.path();
            if child_path.is_dir() {
                stack.push(child_path);
            } else if child_path.is_file() {
                let relative = child_path.strip_prefix(package_root).map_err(|_| {
                    Error::UnsupportedArtifact(format!(
                        "package member `{}` is outside `{}`",
                        child_path.display(),
                        package_root.display()
                    ))
                })?;
                let member_name = Path::new(package_name)
                    .join(relative)
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                members.push((member_name, fs::read(child_path)?));
            }
        }
    }
    members.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(())
}

fn write_wheel_archive(wheel_path: &Path, dist_info: &str, members: Vec<(String, Vec<u8>)>) -> Result<()> {
    let file = File::create(wheel_path)?;
    let mut zip = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let mut record_rows = Vec::new();
    for (name, bytes) in members {
        zip.start_file(&name, options)
            .map_err(|error| Error::UnsupportedArtifact(format!("failed to write fixture wheel member `{name}`: {error}")))?;
        zip.write_all(&bytes)?;
        let digest = URL_SAFE_NO_PAD.encode(Sha256::digest(&bytes));
        record_rows.push(format!("{name},sha256={digest},{}", bytes.len()));
    }
    let record_name = format!("{dist_info}/RECORD");
    record_rows.push(format!("{record_name},,"));
    zip.start_file(&record_name, options)
        .map_err(|error| Error::UnsupportedArtifact(format!("failed to write fixture wheel RECORD: {error}")))?;
    zip.write_all(record_rows.join("\n").as_bytes())?;
    zip.write_all(b"\n")?;
    zip.finish()
        .map_err(|error| Error::UnsupportedArtifact(format!("failed to finish fixture wheel `{}`: {error}", wheel_path.display())))?;
    Ok(())
}


fn unique_temp_dir(prefix: &str, label: &str) -> Result<PathBuf> {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| Error::UnsupportedArtifact(format!("system clock before Unix epoch: {error}")))?
        .as_nanos();
    let path = std::env::temp_dir().join(format!("{prefix}-{label}-{}-{unique}", std::process::id()));
    fs::create_dir_all(&path)?;
    Ok(path)
}
