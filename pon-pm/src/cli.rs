use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::env::EnvLayout;
use crate::error::{Error, Result};
use crate::index::{DEFAULT_INDEX_URL, PackageIndex, SelectedIndex};
use crate::install::{ResolvedRecord, install_package, remove_installed_package};
use crate::lock::LockFile;
use crate::pyproject::PyProject;
use crate::resolve::provider::{PonProvider, ResolveProvider};
use crate::resolve::source::{IndexSource, PackageKind, PackageRecord};
use crate::resolve::{ResolvedArtifact, ResolvedDist, resolve_root};

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProjectOptions {
    manifest_path: PathBuf,
    index_url: Option<String>,
    extra_index_urls: Vec<String>,
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
            let options = parse_project_options(args)?;
            add_command(&options.manifest_path, &requirement, options.index_url.as_deref(), &options.extra_index_urls)
        }
        Some("install") => {
            let options = parse_project_options(args)?;
            install_command(&options.manifest_path, options.index_url.as_deref(), &options.extra_index_urls)
        }
        Some("lock") => {
            let options = parse_project_options(args)?;
            lock_command(&options.manifest_path, options.index_url.as_deref(), &options.extra_index_urls)
        }
        Some("remove") => {
            let name = args
                .next()
                .ok_or_else(|| Error::Cli(format!("missing package name\n{}", usage(&program))))?;
            let manifest = parse_manifest_flag(args)?;
            remove_command(&manifest, &name)
        }
        Some("list") => {
            let manifest = parse_manifest_flag(args)?;
            for dependency in PyProject::read(&manifest)?.dependencies() {
                println!("{dependency}");
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
    let mut pyproject = PyProject::read(&manifest_path)?;
    let dependencies = pyproject.dependencies();
    pyproject.set_dependency_strings(dependencies.iter().map(String::as_str));
    pyproject.write()?;
    layout.create_dirs()?;
    println!("initialized {}", manifest_path.display());
    Ok(())
}

fn add_command(manifest_path: &Path, requirement: &str, index_url: Option<&str>, extra_index_urls: &[String]) -> Result<()> {
    let layout = layout_for_manifest(manifest_path);
    let index = selected_index(manifest_path, &layout, index_url, extra_index_urls)?;
    let resolved = resolve_requirement(&index, requirement)?;
    reject_cabi_package(&resolved)?;

    let mut manifest = PyProject::read(manifest_path)?;
    let changed = manifest.add_dependency(requirement)?;
    manifest.write()?;

    let dependencies = resolve_manifest(&manifest, &index)?;
    install_dependencies(&layout, &dependencies, &index)?;
    write_lock(&layout, &dependencies)?;

    if changed {
        println!("updated {}", manifest_path.display());
    } else {
        println!("replaced {}", manifest_path.display());
    }
    Ok(())
}

fn install_command(manifest_path: &Path, index_url: Option<&str>, extra_index_urls: &[String]) -> Result<()> {
    let manifest = PyProject::read(manifest_path)?;
    let layout = layout_for_manifest(manifest_path);
    let index = selected_index(manifest_path, &layout, index_url, extra_index_urls)?;
    let dependencies = resolve_manifest(&manifest, &index)?;
    install_dependencies(&layout, &dependencies, &index)?;
    write_lock(&layout, &dependencies)?;
    println!("installed {} package(s)", dependencies.len());
    Ok(())
}

fn remove_command(manifest_path: &Path, name: &str) -> Result<()> {
    let mut manifest = PyProject::read(manifest_path)?;
    let changed = manifest.remove_dependency (name)?;
    manifest.write()?;
    let layout = layout_for_manifest(manifest_path);
    let index = selected_index(manifest_path, &layout, None, &[])?;
    let dependencies = resolve_manifest(&manifest, &index)?;
    let removed = remove_installed_package(&layout, name)?;
    write_lock(&layout, &dependencies)?;
    if changed {
        println!("updated {}", manifest_path.display());
    } else {
        println!("not present in {}", manifest_path.display());
    }
    if removed.is_some() {
        println!("removed {name}");
    } else {
        println!("not installed {name}");
    }
    Ok(())
}

fn lock_command(manifest_path: &Path, index_url: Option<&str>, extra_index_urls: &[String]) -> Result<()> {
    let manifest = PyProject::read(manifest_path)?;
    let layout = layout_for_manifest(manifest_path);
    let index = selected_index(manifest_path, &layout, index_url, extra_index_urls)?;
    let dependencies = resolve_manifest(&manifest, &index)?;
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
        let argv = [String::from("-c")];
        return run_inline_code(&layout, &code, extra_env, &argv);
    }

    if first == "--entry" {
        let entry = args
            .next()
            .ok_or_else(|| Error::Cli("missing entry point after `run --entry`".to_owned()))?;
        let mut entry_args = args.collect::<Vec<_>>();
        if entry_args.first().is_some_and(|arg| arg == "--") {
            entry_args.remove(0);
        }
        let (module, attr) = entry
            .split_once(':')
            .filter(|(module, attr)| !module.is_empty() && !attr.is_empty())
            .ok_or_else(|| Error::Cli("entry point must be `<module>:<attr>`".to_owned()))?;
        let code = format!("import {module} as _pon_entry\n_pon_entry.{attr}()\n");
        let mut argv = Vec::with_capacity(entry_args.len() + 1);
        argv.push(entry);
        argv.extend(entry_args);
        return run_inline_code(&layout, &code, extra_env, &argv);
    }

    let mut argv = Vec::with_capacity(args.size_hint().0 + 1);
    argv.push(first.clone());
    argv.extend(args);
    pon_cli::run_file_with_env(first.as_str(), extra_env, &argv).map_err(|error| Error::Cli(format!("{error:#}")))
}

fn run_inline_code(
    layout: &EnvLayout,
    code: &str,
    extra_env: Vec<(OsString, OsString)>,
    argv: &[String],
) -> Result<()> {
    let run_dir = layout.pon_dir.join("run");
    fs::create_dir_all(&run_dir)?;
    let counter = INLINE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = run_dir.join(format!("inline-{}-{counter}.py", std::process::id()));
    fs::write(&path, code)?;
    let result = pon_cli::run_file_with_env(&path, extra_env, argv)
        .map_err(|error| Error::Cli(format!("{error:#}")));
    let _ = fs::remove_file(path);
    result
}

fn resolve_manifest(manifest: &PyProject, index: &impl PackageIndex) -> Result<Vec<ResolvedDist>> {
    let requirements = manifest.dependencies();
    let legacy_dependencies = ResolveProvider::new(index).resolve_requirements(requirements.iter().map(String::as_str))?;
    for dependency in &legacy_dependencies {
        reject_cabi_package(&dependency.record)?;
    }

    let dependencies = resolve_dists(index, requirements.iter().map(String::as_str))?;
    for dependency in &dependencies {
        reject_cabi_dist(dependency)?;
    }
    Ok(dependencies)
}

fn resolve_dists<'a>(index: &impl PackageIndex, requirements: impl IntoIterator<Item = &'a str>) -> Result<Vec<ResolvedDist>> {
    let source = IndexSource::new(index);
    let provider = PonProvider::from_requirements(&source, requirements)?;
    resolve_root(&provider).map(|resolution| resolution.dists)
}

fn resolve_requirement(index: &impl PackageIndex, requirement: &str) -> Result<PackageRecord> {
    ResolveProvider::new(index).resolve_input(requirement, "")
}

fn install_dependencies(
    layout: &EnvLayout,
    dependencies: &[ResolvedDist],
    index: &impl PackageIndex,
) -> Result<()> {
    for dependency in dependencies {
        let install_record = install_record_for(dependency, index)?;
        install_package(layout, &install_record)?;
    }
    Ok(())
}

fn install_record_for(dist: &ResolvedDist, index: &impl PackageIndex) -> Result<ResolvedRecord> {
    if dist.kind.is_refused() {
        return Err(cabi_error(&package_record_from_dist(dist)));
    }

    let version = dist.version.to_string();
    match &dist.artifact {
        ResolvedArtifact::Wheel(file) => {
            let path = index.fetch_artifact(file)?;
            Ok(ResolvedRecord::wheel(dist.name.clone(), version, path))
        }
        ResolvedArtifact::Sdist(file) => {
            let path = index.fetch_artifact(file)?;
            Ok(ResolvedRecord::sdist(dist.name.clone(), version, path))
        }
        ResolvedArtifact::Dir { path, editable } => {
            Ok(ResolvedRecord::dir(dist.name.clone(), version, path.clone(), *editable))
        }
        ResolvedArtifact::Vcs { dir, .. } => Ok(ResolvedRecord::dir(dist.name.clone(), version, dir.clone(), false)),
    }
}


fn reject_cabi_package(record: &PackageRecord) -> Result<()> {
    if record.kind.is_refused() {
        Err(cabi_error(record))
    } else {
        Ok(())
    }
}

fn reject_cabi_dist(dist: &ResolvedDist) -> Result<()> {
    reject_cabi_package(&package_record_from_dist(dist))
}

fn package_record_from_dist(dist: &ResolvedDist) -> PackageRecord {
    PackageRecord {
        name: dist.name.clone(),
        version: dist.version.to_string(),
        kind: dist.kind.clone(),
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

fn write_lock(layout: &EnvLayout, dependencies: &[ResolvedDist]) -> Result<()> {
    fs::create_dir_all(&layout.project_root)?;
    let records = dependencies
        .iter()
        .map(package_record_from_dist)
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

fn parse_project_options(args: impl Iterator<Item = String>) -> Result<ProjectOptions> {
    let mut options = ProjectOptions {
        manifest_path: PathBuf::from("pyproject.toml"),
        index_url: None,
        extra_index_urls: Vec::new(),
    };
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--manifest" | "--pyproject" => {
                let value = args
                    .next()
                    .ok_or_else(|| Error::Cli("missing path after --manifest".to_owned()))?;
                options.manifest_path = PathBuf::from(value);
            }
            "--index-url" => {
                let value = args
                    .next()
                    .ok_or_else(|| Error::Cli("missing URL after --index-url".to_owned()))?;
                options.index_url = Some(value);
            }
            "--extra-index-url" => {
                let value = args
                    .next()
                    .ok_or_else(|| Error::Cli("missing URL after --extra-index-url".to_owned()))?;
                options.extra_index_urls.push(value);
            }
            _ => return Err(Error::Cli(format!("unexpected argument `{arg}`"))),
        }
    }
    Ok(options)
}

fn selected_index(
    manifest_path: &Path,
    layout: &EnvLayout,
    index_url: Option<&str>,
    extra_index_urls: &[String],
) -> Result<SelectedIndex> {
    let pon_home = env::var_os("PON_HOME").map_or_else(|| layout.pon_dir.clone(), PathBuf::from);
    let configured_url = index_url
        .map(str::to_owned)
        .or_else(|| env::var("PON_INDEX_URL").ok().filter(|url| !url.trim().is_empty()));
    let base_url = match configured_url {
        Some(url) => url,
        None => PyProject::read(manifest_path)?
            .tool_pon_index_url()
            .map_or_else(|| DEFAULT_INDEX_URL.to_owned(), str::to_owned),
    };
    if base_url == "catalog:" {
        Ok(SelectedIndex::catalog())
    } else {
        let mut index_urls = Vec::with_capacity(extra_index_urls.len() + 1);
        index_urls.push(base_url);
        index_urls.extend(extra_index_urls.iter().cloned());
        Ok(SelectedIndex::simple_json(index_urls, pon_home))
    }
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
        "usage: {program} init [--manifest <pyproject.toml>]\n       {program} add <requirement-or-path> [--manifest <pyproject.toml>] [--index-url <url>] [--extra-index-url <url> ...]\n       {program} install [--manifest <pyproject.toml>] [--index-url <url>] [--extra-index-url <url> ...]\n       {program} lock [--manifest <pyproject.toml>] [--index-url <url>] [--extra-index-url <url> ...]\n       {program} run <file> [args...]\n       {program} run -c <code>\n       {program} run --entry <module:attr> -- [args...]\n       {program} remove <name> [--manifest <pyproject.toml>]\n       {program} list [--manifest <pyproject.toml>]\n       {program} env [project-root]"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::read_installed_packages;

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
    fn project_options_accept_index_urls_for_resolving_commands() {
        let args = [
            "--manifest".to_owned(),
            "demo.toml".to_owned(),
            "--index-url".to_owned(),
            "https://packages.example/simple/".to_owned(),
            "--extra-index-url".to_owned(),
            "https://mirror-one.example/simple/".to_owned(),
            "--extra-index-url".to_owned(),
            "https://mirror-two.example/simple/".to_owned(),
        ]
        .into_iter();
        let options = parse_project_options(args).expect("options");

        assert_eq!(options.manifest_path, PathBuf::from("demo.toml"));
        assert_eq!(options.index_url.as_deref(), Some("https://packages.example/simple/"));
        assert_eq!(
            options.extra_index_urls,
            vec![
                "https://mirror-one.example/simple/".to_owned(),
                "https://mirror-two.example/simple/".to_owned()
            ]
        );
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
            "--index-url".to_owned(),
            "catalog:".to_owned(),
        ])
        .expect("add");

        let pyproject = fs::read_to_string(&manifest).expect("pyproject");
        assert!(pyproject.contains("\"idna\""));
        assert!(root.join(".pon/packages/site-packages/idna/__init__.py").is_file());
        let lock = fs::read_to_string(root.join("pon.lock")).expect("lock");
        assert!(lock.contains("name = \"idna\""));
        assert!(lock.contains("version = \"3.10\""));
    }

    #[test]
    fn add_local_sdist_records_raw_path_and_installs_package() {
        let root = temp_project("add-sdist");
        let manifest = root.join("pyproject.toml");
        fs::create_dir_all(&root).expect("root");
        let sdist_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join("sdists")
            .join("pon-flit-fixture-0.1.0.tar.gz");
        let raw_path = sdist_path.display().to_string();

        run_from_args([
            "pon-pm".to_owned(),
            "add".to_owned(),
            raw_path.clone(),
            "--manifest".to_owned(),
            manifest.display().to_string(),
            "--index-url".to_owned(),
            "catalog:".to_owned(),
        ])
        .expect("add local sdist");

        let pyproject = PyProject::read(&manifest).expect("manifest");
        let dependencies = pyproject.dependencies();
        assert_eq!(dependencies.len(), 1);
        assert_eq!(dependencies[0], raw_path);
        let input = crate::requirement::parse_requirement_input(&dependencies[0]).expect("requirement");
        let normalized_name = crate::requirement::normalized_name_of(&input, root.as_path()).expect("normalized name");
        assert_eq!(normalized_name, "pon-flit-fixture");

        let package_init = root.join(".pon/packages/site-packages/pon_flit_fixture/__init__.py");
        assert_eq!(
            fs::read_to_string(&package_init).expect("installed package"),
            "__version__ = \"0.1.0\"\n"
        );

        let registry = read_installed_packages(&EnvLayout::new(&root)).expect("registry");
        assert_eq!(registry.len(), 1);
        assert_eq!(registry[0].name, "pon-flit-fixture");
        assert_eq!(registry[0].version, "0.1.0");
        assert_eq!(registry[0].artifact_kind, "wheel");
        assert_eq!(registry[0].import_names, vec!["pon_flit_fixture"]);
    }

    #[test]
    fn remove_deletes_installed_wheel_files_and_registry_row() {
        let root = temp_project("remove");
        let manifest = root.join("pyproject.toml");
        fs::create_dir_all(&root).expect("root");
        run_from_args([
            "pon-pm".to_owned(),
            "add".to_owned(),
            "idna".to_owned(),
            "--manifest".to_owned(),
            manifest.display().to_string(),
            "--index-url".to_owned(),
            "catalog:".to_owned(),
        ])
        .expect("add");

        run_from_args([
            "pon-pm".to_owned(),
            "remove".to_owned(),
            "idna".to_owned(),
            "--manifest".to_owned(),
            manifest.display().to_string(),
        ])
        .expect("remove");

        let pyproject = fs::read_to_string(&manifest).expect("pyproject");
        assert!(!pyproject.contains("\"idna\""));
        assert!(!root.join(".pon/packages/site-packages/idna").exists());
        assert!(!root.join(".pon/packages/site-packages/idna-3.10.dist-info").exists());
        let registry = fs::read_to_string(root.join(".pon/packages/installed.tsv")).expect("registry");
        assert!(registry.is_empty());
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
            "--index-url".to_owned(),
            "catalog:".to_owned(),
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
