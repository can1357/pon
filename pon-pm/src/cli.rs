use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::env::EnvLayout;
use crate::error::{Error, Result};
use crate::index::CatalogIndex;
use crate::install::{ResolvedRecord, install_package};
use crate::lock::LockFile;
use crate::manifest::{ProjectManifest, Requirement, remove_dependency};
use crate::resolve::provider::ResolveProvider;
use crate::resolve::source::{PackageKind, PackageRecord, PackageSource};

struct ResolvedDependency {
    raw: String,
    record: PackageRecord,
}

static INLINE_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn run_from_env() -> Result<()> {
    run_from_args(env::args())
}

pub fn run_from_args(args: impl IntoIterator<Item = String>) -> Result<()> {
    let mut args = args.into_iter();
    let program = args.next().unwrap_or_else(|| "pon-pm".to_owned());
    match args.next().as_deref() {
        Some("init") => init_command(args),
        Some("run") => run_command(args),
        Some("add") => {
            let requirement = args
                .next()
                .ok_or_else(|| Error::Cli(format!("missing requirement\n{}", usage(&program))))?;
            let manifest = parse_manifest_flag(args)?;
            add_command(&manifest, &requirement)
        }
        Some("install") => {
            let manifest = parse_manifest_flag(args)?;
            install_command(&manifest)
        }
        Some("lock") => {
            let manifest = parse_manifest_flag(args)?;
            lock_command(&manifest)
        }
        Some("remove") => {
            let name = args
                .next()
                .ok_or_else(|| Error::Cli(format!("missing package name\n{}", usage(&program))))?;
            let manifest = parse_manifest_flag(args)?;
            if remove_dependency(&manifest, name)? {
                println!("updated {}", manifest.display());
            } else {
                println!("not present in {}", manifest.display());
            }
            Ok(())
        }
        Some("list") => {
            let manifest = parse_manifest_flag(args)?;
            for dependency in ProjectManifest::read(&manifest)?.dependencies() {
                println!("{}", dependency.raw());
            }
            Ok(())
        }
        Some("env") => {
            let root = args.next().map_or_else(|| PathBuf::from("."), PathBuf::from);
            reject_extra(args.next(), "env")?;
            let layout = EnvLayout::new(root);
            println!("PON_HOME={}", layout.pon_dir.display());
            println!("PONPATH={}", layout.import_path_string());
            println!("PON_IMPORT_PATH={}", layout.import_path_string());
            println!("PON_NATIVE_MODULE_REGISTRY={}", layout.native_registry_path.display());
            Ok(())
        }
        Some(command) => Err(Error::Cli(format!("unknown subcommand `{command}`\n{}", usage(&program)))),
        None => Err(Error::Cli(usage(program))),
    }
}

fn init_command(args: impl Iterator<Item = String>) -> Result<()> {
    let manifest_path = parse_manifest_flag(args)?;
    let layout = layout_for_manifest(&manifest_path);
    let manifest = ProjectManifest::read(&manifest_path)?;
    manifest.write()?;
    layout.create_dirs()?;
    println!("initialized {}", manifest_path.display());
    Ok(())
}

fn add_command(manifest_path: &Path, requirement: &str) -> Result<()> {
    let resolved = resolve_requirement(requirement)?;
    reject_cabi_package(&resolved)?;

    let mut manifest = ProjectManifest::read(manifest_path)?;
    let changed = manifest.add(Requirement::for_resolved_package(requirement, &resolved.name)?);
    manifest.write()?;

    let dependencies = resolve_manifest(&manifest)?;
    let layout = layout_for_manifest(manifest_path);
    install_dependencies(&layout, &dependencies)?;
    write_lock(&layout, &dependencies)?;

    if changed {
        println!("updated {}", manifest_path.display());
    } else {
        println!("replaced {}", manifest_path.display());
    }
    Ok(())
}

fn install_command(manifest_path: &Path) -> Result<()> {
    let manifest = ProjectManifest::read(manifest_path)?;
    let dependencies = resolve_manifest(&manifest)?;
    let layout = layout_for_manifest(manifest_path);
    install_dependencies(&layout, &dependencies)?;
    write_lock(&layout, &dependencies)?;
    println!("installed {} package(s)", dependencies.len());
    Ok(())
}

fn lock_command(manifest_path: &Path) -> Result<()> {
    let manifest = ProjectManifest::read(manifest_path)?;
    let dependencies = resolve_manifest(&manifest)?;
    let layout = layout_for_manifest(manifest_path);
    write_lock(&layout, &dependencies)?;
    println!("wrote {}", layout.project_root.join("pon.lock").display());
    Ok(())
}

fn run_command(mut args: impl Iterator<Item = String>) -> Result<()> {
    let first = args
        .next()
        .ok_or_else(|| Error::Cli("missing file or `-c <code>` for `run`".to_owned()))?;
    let layout = discover_layout()?;
    layout.create_dirs()?;
    let extra_env = runtime_env(&layout);

    if first == "-c" {
        let code = args
            .next()
            .ok_or_else(|| Error::Cli("missing code after `run -c`".to_owned()))?;
        reject_extra(args.next(), "run")?;
        return run_inline_code(&layout, &code, extra_env);
    }

    reject_extra(args.next(), "run")?;
    pon_cli::run_file_with_env(first, extra_env).map_err(|error| Error::Cli(format!("{error:#}")))
}

fn run_inline_code(layout: &EnvLayout, code: &str, extra_env: Vec<(OsString, OsString)>) -> Result<()> {
    let run_dir = layout.pon_dir.join("run");
    fs::create_dir_all(&run_dir)?;
    let counter = INLINE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = run_dir.join(format!("inline-{}-{counter}.py", std::process::id()));
    fs::write(&path, code)?;
    let result = pon_cli::run_file_with_env(&path, extra_env).map_err(|error| Error::Cli(format!("{error:#}")));
    let _ = fs::remove_file(path);
    result
}

fn resolve_manifest(manifest: &ProjectManifest) -> Result<Vec<ResolvedDependency>> {
    manifest
        .dependencies()
        .into_iter()
        .map(|requirement| {
            let resolved = resolve_requirement(requirement.raw())?;
            reject_cabi_package(&resolved)?;
            Ok(ResolvedDependency {
                raw: requirement.raw().to_owned(),
                record: resolved,
            })
        })
        .collect()
}

fn resolve_requirement(requirement: &str) -> Result<PackageRecord> {
    let (source, specifier) = split_requirement(requirement);
    ResolveProvider::default().resolve_input(source, specifier)
}

fn split_requirement(requirement: &str) -> (&str, &str) {
    let requirement = requirement.trim();
    if PackageSource::parse(requirement).is_ok_and(|source| matches!(source, PackageSource::Path(_))) {
        return (requirement, "");
    }
    if let Some(index) = requirement.char_indices().find_map(|(index, ch)| {
        if matches!(ch, '<' | '>' | '=' | '!' | '~') || ch.is_whitespace() {
            Some(index)
        } else {
            None
        }
    }) {
        (requirement[..index].trim(), requirement[index..].trim())
    } else {
        (requirement, "")
    }
}

fn install_dependencies(layout: &EnvLayout, dependencies: &[ResolvedDependency]) -> Result<()> {
    for dependency in dependencies {
        let install_record = install_record_for(&dependency.raw, &dependency.record)?;
        install_package(layout, &install_record)?;
    }
    Ok(())
}

fn install_record_for(requirement: &str, record: &PackageRecord) -> Result<ResolvedRecord> {
    match &record.kind {
        PackageKind::Pure => {
            let filename = catalog_filename(&record.name, &record.version)?;
            Ok(ResolvedRecord::wheel(&record.name, &record.version, filename))
        }
        PackageKind::Native => {
            if matches!(PackageSource::parse(requirement)?, PackageSource::Path(_)) {
                Ok(ResolvedRecord::local_path(&record.name, &record.version, requirement))
            } else {
                Err(Error::UnsupportedArtifact(format!(
                    "native package `{}` must be installed from a local path",
                    record.name
                )))
            }
        }
        PackageKind::CAbiRefused { .. } => Err(cabi_error(record)),
    }
}

fn catalog_filename(name: &str, version: &str) -> Result<String> {
    let index = CatalogIndex::new();
    let project = index
        .lookup(name)?
        .ok_or_else(|| Error::InvalidRequirement(format!("unknown package `{name}`")))?;
    project
        .files
        .into_iter()
        .find(|file| file.version.raw() == version && matches!(file.kind, PackageKind::Pure))
        .map(|file| file.filename)
        .ok_or_else(|| Error::UnsupportedArtifact(format!("no installable pure-Python artifact for `{name}` {version}")))
}

fn reject_cabi_package(record: &PackageRecord) -> Result<()> {
    if record.kind.is_refused() {
        Err(cabi_error(record))
    } else {
        Ok(())
    }
}

fn cabi_error(record: &PackageRecord) -> Error {
    let reason = match &record.kind {
        PackageKind::CAbiRefused { reason } => format!(": {reason}"),
        _ => String::new(),
    };
    Error::UnsupportedArtifact(format!(
        "package `{}` requires the CPython C-ABI (ob_refcnt){reason}; this is a by-design limitation of pon",
        record.name
    ))
}

fn write_lock(layout: &EnvLayout, dependencies: &[ResolvedDependency]) -> Result<()> {
    fs::create_dir_all(&layout.project_root)?;
    let records = dependencies
        .iter()
        .map(|dependency| dependency.record.clone())
        .collect::<Vec<_>>();
    LockFile::from_records(&records).write_to_path(layout.project_root.join("pon.lock"))
}

fn layout_for_manifest(manifest_path: &Path) -> EnvLayout {
    let root = manifest_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    EnvLayout::new(root)
}

fn discover_layout() -> Result<EnvLayout> {
    let cwd = env::current_dir()?;
    for ancestor in cwd.ancestors() {
        if ancestor.join("pyproject.toml").is_file() || ancestor.join(".pon").is_dir() {
            return Ok(EnvLayout::new(ancestor));
        }
    }
    Ok(EnvLayout::new(cwd))
}

fn runtime_env(layout: &EnvLayout) -> Vec<(OsString, OsString)> {
    let import_path = OsString::from(layout.import_path_string());
    vec![
        (OsString::from("PON_HOME"), layout.pon_dir.clone().into_os_string()),
        (OsString::from("PONPATH"), import_path.clone()),
        (OsString::from("PON_IMPORT_PATH"), import_path),
        (
            OsString::from("PON_NATIVE_MODULE_REGISTRY"),
            layout.native_registry_path.clone().into_os_string(),
        ),
    ]
}

fn parse_manifest_flag(args: impl Iterator<Item = String>) -> Result<PathBuf> {
    let mut manifest = PathBuf::from("pyproject.toml");
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--manifest" | "--pyproject" => {
                let value = args
                    .next()
                    .ok_or_else(|| Error::Cli("missing path after --manifest".to_owned()))?;
                manifest = PathBuf::from(value);
            }
            _ => return Err(Error::Cli(format!("unexpected argument `{arg}`"))),
        }
    }
    Ok(manifest)
}

fn reject_extra(extra: Option<String>, command: &str) -> Result<()> {
    if let Some(extra) = extra {
        Err(Error::Cli(format!("unexpected argument `{extra}` for `{command}`")))
    } else {
        Ok(())
    }
}

fn usage(program: impl AsRef<str>) -> String {
    let program = program.as_ref();
    format!(
        "usage: {program} init [--manifest <pyproject.toml>]\n       {program} add <requirement-or-path> [--manifest <pyproject.toml>]\n       {program} install [--manifest <pyproject.toml>]\n       {program} lock [--manifest <pyproject.toml>]\n       {program} run <file>\n       {program} run -c <code>\n       {program} remove <name> [--manifest <pyproject.toml>]\n       {program} list [--manifest <pyproject.toml>]\n       {program} env [project-root]"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_project(name: &str) -> PathBuf {
        let unique = format!(
            "pon-pm-cli-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        );
        std::env::temp_dir().join(unique)
    }

    #[test]
    fn manifest_flag_defaults_to_pyproject() {
        assert_eq!(parse_manifest_flag(std::iter::empty()).expect("flag"), PathBuf::from("pyproject.toml"));
    }

    #[test]
    fn manifest_flag_accepts_explicit_path() {
        let args = ["--manifest".to_owned(), "demo.toml".to_owned()].into_iter();
        assert_eq!(parse_manifest_flag(args).expect("flag"), PathBuf::from("demo.toml"));
    }

    #[test]
    fn init_creates_manifest_and_dot_pon_dirs() {
        let root = temp_project("init");
        let manifest = root.join("pyproject.toml");

        run_from_args([
            "pon-pm".to_owned(),
            "init".to_owned(),
            "--manifest".to_owned(),
            manifest.display().to_string(),
        ])
        .expect("init");

        let content = fs::read_to_string(&manifest).expect("manifest");
        assert!(content.contains("[project]"));
        assert!(content.contains("dependencies = ["));
        assert!(root.join(".pon/packages/site-packages").is_dir());
        assert!(root.join(".pon/native").is_dir());
    }

    #[test]
    fn add_resolves_installs_and_locks_catalog_package() {
        let root = temp_project("add");
        let manifest = root.join("pyproject.toml");
        fs::create_dir_all(&root).expect("root");

        run_from_args([
            "pon-pm".to_owned(),
            "add".to_owned(),
            "idna".to_owned(),
            "--manifest".to_owned(),
            manifest.display().to_string(),
        ])
        .expect("add");

        let pyproject = fs::read_to_string(&manifest).expect("pyproject");
        assert!(pyproject.contains("\"idna\""));
        assert!(root.join(".pon/packages/site-packages/idna.py").is_file());
        let lock = fs::read_to_string(root.join("pon.lock")).expect("lock");
        assert!(lock.contains("name = \"idna\""));
        assert!(lock.contains("version = \"3.10\""));
    }

    #[test]
    fn add_refuses_c_abi_catalog_package() {
        let root = temp_project("numpy");
        let manifest = root.join("pyproject.toml");
        fs::create_dir_all(&root).expect("root");

        let error = run_from_args([
            "pon-pm".to_owned(),
            "add".to_owned(),
            "numpy".to_owned(),
            "--manifest".to_owned(),
            manifest.display().to_string(),
        ])
        .expect_err("numpy must be refused");
        let message = error.to_string();
        assert!(message.contains("requires the CPython C-ABI"));
        assert!(message.contains("ob_refcnt"));
        assert!(message.contains("by-design limitation of pon"));
        assert!(!manifest.exists());
    }

    #[test]
    fn runtime_env_exports_import_and_native_registry_paths() {
        let layout = EnvLayout::new("/tmp/project");
        let env = runtime_env(&layout)
            .into_iter()
            .map(|(key, value)| (key.to_string_lossy().into_owned(), value.to_string_lossy().into_owned()))
            .collect::<std::collections::BTreeMap<_, _>>();

        assert_eq!(env["PON_HOME"], "/tmp/project/.pon");
        assert!(env["PONPATH"].contains("/tmp/project/.pon/packages/site-packages"));
        assert_eq!(env["PONPATH"], env["PON_IMPORT_PATH"]);
        assert_eq!(env["PON_NATIVE_MODULE_REGISTRY"], "/tmp/project/.pon/native/registry.tsv");
    }
}
