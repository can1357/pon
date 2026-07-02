use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use clap::{Args, Parser, Subcommand};
use pep440_rs::{Operator, Version, VersionSpecifiers};
use pep508_rs::{Requirement, VersionOrUrl};
use sha2::{Digest, Sha256};

use crate::env::EnvLayout;
use crate::error::{Error, Result};
use crate::index::download::{download_artifact, HashPolicy};
use crate::index::{DEFAULT_INDEX_URL, PackageIndex, SelectedIndex};
use crate::install::{
    install_package, read_installed_packages, remove_installed_package, InstalledPackageRecord, ResolvedRecord,
};
use crate::lock::{compute_input_hash, missing_frozen_lock_error, stale_lock_error, LockFile, DEFAULT_REQUIRES_PYTHON};
use crate::marker::pon_marker_env;
use crate::metadata::{parse_core_metadata, CoreMetadata};
use crate::names;
use crate::pyproject::{PonSource, PyProject};
use crate::requirement::{normalized_name_of, parse_requirement_input, RequirementInput};
use crate::requirements::{parse_requirements_file, RequirementEntry, RequirementsFile};
use crate::resolve::provider::PonProvider;
use crate::resolve::source::{IndexSource, PackageKind, PackageRecord};
use crate::resolve::{resolve_root, Resolution, ResolvedArtifact, ResolvedDist};

static INLINE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Parser)]
#[command(name = "pon-pm", disable_help_subcommand = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Init(ManifestArgs),
    Add(AddArgs),
    Remove(RemoveArgs),
    Install(InstallArgs),
    Lock(LockArgs),
    Run(RunArgs),
    List(ManifestArgs),
    Freeze,
    Show(ShowArgs),
    Download(DownloadArgs),
    Check,
    Cache(CacheArgs),
    Env(EnvArgs),
}

#[derive(Clone, Debug, Args)]
struct ManifestArgs {
    #[arg(long, alias = "pyproject", value_name = "pyproject.toml", default_value = "pyproject.toml")]
    manifest: PathBuf,
}

#[derive(Clone, Debug, Args, Default)]
struct IndexArgs {
    #[arg(long, value_name = "url")]
    index_url: Option<String>,
    #[arg(long, value_name = "url")]
    extra_index_url: Vec<String>,
}

#[derive(Debug, Args)]
struct AddArgs {
    #[arg(value_name = "req", required = true)]
    requirements: Vec<String>,
    #[arg(long)]
    editable: bool,
    #[arg(long)]
    pre: bool,
    #[command(flatten)]
    manifest: ManifestArgs,
    #[command(flatten)]
    index: IndexArgs,
}

#[derive(Debug, Args)]
struct RemoveArgs {
    #[arg(value_name = "name", required = true)]
    names: Vec<String>,
    #[command(flatten)]
    manifest: ManifestArgs,
}

#[derive(Debug, Args)]
struct InstallArgs {
    #[arg(value_name = "req")]
    requirements: Vec<String>,
    #[arg(short = 'r', long = "requirement", value_name = "file")]
    requirement_files: Vec<PathBuf>,
    #[arg(short = 'c', long = "constraint", value_name = "file")]
    constraint_files: Vec<PathBuf>,
    #[arg(short = 'e', long = "editable", value_name = "path")]
    editables: Vec<PathBuf>,
    #[arg(long)]
    no_deps: bool,
    #[arg(long)]
    pre: bool,
    #[arg(long)]
    require_hashes: bool,
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    frozen: bool,
    #[arg(long)]
    no_index: bool,
    #[command(flatten)]
    manifest: ManifestArgs,
    #[command(flatten)]
    index: IndexArgs,
}

#[derive(Debug, Args)]
struct LockArgs {
    #[command(flatten)]
    manifest: ManifestArgs,
    #[command(flatten)]
    index: IndexArgs,
}

#[derive(Debug, Args)]
struct RunArgs {
    #[arg(short = 'c', value_name = "code")]
    code: Option<String>,
    #[arg(long, value_name = "module:attr")]
    entry: Option<String>,
    #[arg(value_name = "args", trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

#[derive(Debug, Args)]
struct ShowArgs {
    #[arg(value_name = "name", required = true)]
    names: Vec<String>,
}

#[derive(Debug, Args)]
struct DownloadArgs {
    #[arg(value_name = "req")]
    requirements: Vec<String>,
    #[arg(short = 'd', long = "dest", value_name = "dir")]
    dest: PathBuf,
    #[arg(short = 'r', long = "requirement", value_name = "file")]
    requirement_files: Vec<PathBuf>,
    #[arg(short = 'c', long = "constraint", value_name = "file")]
    constraint_files: Vec<PathBuf>,
    #[arg(long)]
    pre: bool,
    #[arg(long)]
    require_hashes: bool,
    #[arg(long)]
    no_deps: bool,
    #[arg(long)]
    no_index: bool,
    #[command(flatten)]
    manifest: ManifestArgs,
    #[command(flatten)]
    index: IndexArgs,
}

#[derive(Debug, Args)]
struct CacheArgs {
    #[command(subcommand)]
    action: CacheAction,
}

#[derive(Debug, Subcommand)]
enum CacheAction {
    Dir,
    Purge,
}

#[derive(Debug, Args)]
struct EnvArgs {
    #[arg(value_name = "project-root")]
    root: Option<PathBuf>,
}

#[derive(Clone, Debug)]
struct RequirementSpec {
    raw: String,
    input: RequirementInput,
    hashes: Vec<String>,
}

#[derive(Clone, Debug, Default)]
struct CollectedInputs {
    requirements: Vec<RequirementSpec>,
    constraints: Vec<PathBuf>,
    index_url: Option<String>,
    extra_index_urls: Vec<String>,
    no_index: bool,
    pre: bool,
    require_hashes: bool,
}

#[derive(Clone, Debug, Default)]
struct ResolveOptions {
    allow_prerelease: bool,
    no_deps: bool,
    constraints: HashMap<String, VersionSpecifiers>,
}

pub fn run_from_env() -> Result<()> {
    run_from_args(env::args())
}

pub fn run_from_args(args: impl IntoIterator<Item = String>) -> Result<()> {
    let cli = Cli::try_parse_from(args).map_err(|error| Error::Cli(error.to_string().trim_end().to_owned()))?;
    match cli.command {
        Command::Init(args) => init_command(args),
        Command::Add(args) => add_command(args),
        Command::Remove(args) => remove_command(args),
        Command::Install(args) => install_command(args),
        Command::Lock(args) => lock_command(args),
        Command::Run(args) => run_command(args),
        Command::List(args) => list_command(args.manifest.as_path()),
        Command::Freeze => freeze_command(),
        Command::Show(args) => show_command(&args.names),
        Command::Download(args) => download_command(args),
        Command::Check => check_command(),
        Command::Cache(args) => cache_command(args),
        Command::Env(args) => env_command(args.root),
    }
}

fn init_command(args: ManifestArgs) -> Result<()> {
    let manifest_path = args.manifest;
    let layout = layout_for_manifest(&manifest_path);
    let mut pyproject = PyProject::read(&manifest_path)?;
    let dependencies = pyproject.dependencies();
    pyproject.set_dependency_strings(dependencies.iter().map(String::as_str));
    pyproject.write()?;
    layout.create_dirs()?;
    println!("initialized {}", manifest_path.display());
    Ok(())
}

fn add_command(args: AddArgs) -> Result<()> {
    let manifest_path = args.manifest.manifest;
    let layout = layout_for_manifest(&manifest_path);
    let manifest = PyProject::read(&manifest_path)?;
    let allow_prerelease = args.pre || manifest.tool_pon_allow_prerelease();
    let index = selected_index(
        &manifest_path,
        &layout,
        args.index.index_url.as_deref(),
        &args.index.extra_index_url,
        false,
    )?;
    let requirements = args
        .requirements
        .iter()
        .map(|raw| requirement_spec_from_cli(raw, args.editable, &layout.project_root))
        .collect::<Result<Vec<_>>>()?;

    let options = ResolveOptions {
        allow_prerelease,
        no_deps: false,
        constraints: HashMap::new(),
    };
    let added_resolution = resolve_requirement_specs(&index, &requirements, &options, &layout.project_root)?;
    for dist in &added_resolution {
        reject_cabi_dist(dist)?;
    }

    let mut manifest = PyProject::read(&manifest_path)?;
    let mut changed = false;
    for requirement in &requirements {
        changed |= persist_dependency(&mut manifest, requirement, &layout.project_root)?;
    }
    manifest.write()?;

    let dependencies = resolve_manifest(&manifest, &index, &options)?;
    install_dependencies(&layout, &dependencies, &index, None)?;
    let input_hash = manifest_input_hash(&manifest, &manifest_path, &layout, args.index.index_url.as_deref(), false, allow_prerelease)?;
    write_lock(&layout, &dependencies, &input_hash)?;

    if changed {
        println!("updated {}", manifest_path.display());
    } else {
        println!("replaced {}", manifest_path.display());
    }
    Ok(())
}

fn install_command(args: InstallArgs) -> Result<()> {
    let manifest_path = args.manifest.manifest;
    let layout = layout_for_manifest(&manifest_path);
    let manifest = PyProject::read(&manifest_path)?;
    let collected = collect_inputs(&args.requirements, &args.requirement_files, &args.editables, &args.constraint_files)?;
    let explicit_install = !args.requirements.is_empty() || !args.requirement_files.is_empty() || !args.editables.is_empty();
    let index_url = args.index.index_url.clone().or(collected.index_url.clone());
    let extra_index_urls = merged_extra_index_urls(&args.index.extra_index_url, &collected.extra_index_urls);
    let no_index = args.no_index || collected.no_index;
    let allow_prerelease = args.pre || collected.pre || manifest.tool_pon_allow_prerelease();
    let constraints = collect_constraints(&collected.constraints)?;
    let options = ResolveOptions {
        allow_prerelease,
        no_deps: args.no_deps,
        constraints,
    };
    let index = selected_index(&manifest_path, &layout, index_url.as_deref(), &extra_index_urls, no_index)?;
    let hash_mode = args.require_hashes || collected.require_hashes || collected.requirements.iter().any(|req| !req.hashes.is_empty());

    if explicit_install {
        install_explicit_requirements(
            &layout,
            &index,
            &collected.requirements,
            &options,
            hash_mode,
            args.dry_run,
        )
    } else {
        install_project(
            &layout,
            &manifest_path,
            &manifest,
            &index,
            &options,
            index_url.as_deref(),
            no_index,
            hash_mode || args.require_hashes,
            args.dry_run,
            args.frozen,
        )
    }
}

fn install_explicit_requirements(
    layout: &EnvLayout,
    index: &impl PackageIndex,
    requirements: &[RequirementSpec],
    options: &ResolveOptions,
    hash_mode: bool,
    dry_run: bool,
) -> Result<()> {
    let dependencies = resolve_requirement_specs(index, requirements, options, &layout.project_root)?;
    let hash_requirements = enforce_hash_mode(requirements, hash_mode, &layout.project_root, &dependencies)?;
    if dry_run {
        print_dry_run(&dependencies);
        return Ok(());
    }
    install_dependencies(layout, &dependencies, index, hash_requirements.as_ref())?;
    println!("installed {} package(s)", dependencies.len());
    Ok(())
}

fn install_project(
    layout: &EnvLayout,
    manifest_path: &Path,
    manifest: &PyProject,
    index: &impl PackageIndex,
    options: &ResolveOptions,
    index_url: Option<&str>,
    no_index: bool,
    hash_mode: bool,
    dry_run: bool,
    frozen: bool,
) -> Result<()> {
    let requirements = manifest_requirement_specs(manifest)?;
    let input_hash = manifest_input_hash(manifest, manifest_path, layout, index_url, no_index, options.allow_prerelease)?;
    let lock_path = layout.project_root.join("pon.lock");
    if options.constraints.is_empty() && !hash_mode && lock_path.is_file() {
        let lock = LockFile::read_from_path(&lock_path)?;
        if lock.is_stale(&input_hash) {
            if frozen {
                return Err(stale_lock_error());
            }
        } else if dry_run {
            for package in &lock.packages {
                println!("would install {} {}", package.name, package.version);
            }
            return Ok(());
        } else {
            let installed = install_locked_packages(layout, &lock)?;
            println!("installed {installed} package(s)");
            return Ok(());
        }
    } else if frozen && !lock_path.is_file() {
        return Err(missing_frozen_lock_error());
    } else if frozen && lock_path.is_file() {
        LockFile::read_from_path(&lock_path)?.ensure_fresh(&input_hash)?;
    } else if frozen {
        return Err(missing_frozen_lock_error());
    }

    let dependencies = resolve_requirement_specs(index, &requirements, options, &layout.project_root)?;
    let hash_requirements = enforce_hash_mode(&requirements, hash_mode, &layout.project_root, &dependencies)?;
    if dry_run {
        print_dry_run(&dependencies);
        return Ok(());
    }
    install_dependencies(layout, &dependencies, index, hash_requirements.as_ref())?;
    write_lock(layout, &dependencies, &input_hash)?;
    println!("installed {} package(s)", dependencies.len());
    Ok(())
}

fn remove_command(args: RemoveArgs) -> Result<()> {
    let manifest_path = args.manifest.manifest;
    for name in args.names {
        remove_one(&manifest_path, &name)?;
    }
    Ok(())
}

fn remove_one(manifest_path: &Path, name: &str) -> Result<()> {
    let mut manifest = PyProject::read(manifest_path)?;
    let changed = manifest.remove_dependency(name)?;
    manifest.remove_source(name);
    manifest.write()?;
    let layout = layout_for_manifest(manifest_path);
    let manifest = PyProject::read(manifest_path)?;
    let index = selected_index(manifest_path, &layout, None, &[], false)?;
    let options = ResolveOptions {
        allow_prerelease: manifest.tool_pon_allow_prerelease(),
        no_deps: false,
        constraints: HashMap::new(),
    };
    let dependencies = resolve_manifest(&manifest, &index, &options)?;
    let removed = remove_installed_package(&layout, name)?;
    let input_hash = manifest_input_hash(&manifest, manifest_path, &layout, None, false, options.allow_prerelease)?;
    write_lock(&layout, &dependencies, &input_hash)?;
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

fn lock_command(args: LockArgs) -> Result<()> {
    let manifest_path = args.manifest.manifest;
    let manifest = PyProject::read(&manifest_path)?;
    let layout = layout_for_manifest(&manifest_path);
    let index = selected_index(
        &manifest_path,
        &layout,
        args.index.index_url.as_deref(),
        &args.index.extra_index_url,
        false,
    )?;
    let options = ResolveOptions {
        allow_prerelease: manifest.tool_pon_allow_prerelease(),
        no_deps: false,
        constraints: HashMap::new(),
    };
    let dependencies = resolve_manifest(&manifest, &index, &options)?;
    let input_hash = manifest_input_hash(&manifest, &manifest_path, &layout, args.index.index_url.as_deref(), false, options.allow_prerelease)?;
    write_lock(&layout, &dependencies, &input_hash)?;
    println!("wrote {}", layout.project_root.join("pon.lock").display());
    Ok(())
}

fn run_command(mut args: RunArgs) -> Result<()> {
    let layout = discover_layout()?;
    layout.create_dirs()?;
    let extra_env = runtime_env(&layout);

    if let Some(code) = args.code {
        if args.entry.is_some() || !args.args.is_empty() {
            return Err(Error::Cli("unexpected argument for `run -c`".to_owned()));
        }
        let argv = [String::from("-c")];
        return run_inline_code(&layout, &code, extra_env, &argv);
    }

    if let Some(entry) = args.entry {
        if args.args.first().is_some_and(|arg| arg == "--") {
            args.args.remove(0);
        }
        let (module, attr) = entry
            .split_once(':')
            .filter(|(module, attr)| !module.is_empty() && !attr.is_empty())
            .ok_or_else(|| Error::Cli("entry point must be `<module>:<attr>`".to_owned()))?;
        let code = format!("import {module} as _pon_entry\n_pon_entry.{attr}()\n");
        let mut argv = Vec::with_capacity(args.args.len() + 1);
        argv.push(entry);
        argv.extend(args.args);
        return run_inline_code(&layout, &code, extra_env, &argv);
    }

    let Some(file) = args.args.first().cloned() else {
        return Err(Error::Cli("missing file or `-c <code>` for `run`".to_owned()));
    };
    let mut argv = args.args;
    if argv.is_empty() {
        argv.push(file.clone());
    }
    pon_cli::run_file_with_env(file.as_str(), extra_env, &argv).map_err(|error| Error::Cli(format!("{error:#}")))
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
    let result = pon_cli::run_file_with_env(&path, extra_env, argv).map_err(|error| Error::Cli(format!("{error:#}")));
    let _ = fs::remove_file(path);
    result
}

fn list_command(manifest_path: &Path) -> Result<()> {
    let layout = layout_for_manifest(manifest_path);
    let mut packages = read_installed_packages(&layout)?;
    packages.sort_by(|left, right| names::normalize(&left.name).cmp(&names::normalize(&right.name)));
    println!("Package Version");
    for package in packages {
        println!("{} {}", package.name, package.version);
    }
    Ok(())
}

fn freeze_command() -> Result<()> {
    let layout = discover_layout()?;
    let mut packages = read_installed_packages(&layout)?;
    packages.sort_by(|left, right| names::normalize(&left.name).cmp(&names::normalize(&right.name)));
    for package in packages {
        println!("{}=={}", names::normalize(&package.name), package.version);
    }
    Ok(())
}

fn show_command(names_to_show: &[String]) -> Result<()> {
    let layout = discover_layout()?;
    let packages = read_installed_packages(&layout)?;
    let metadata = installed_metadata_map(&layout, &packages)?;
    for (index, requested) in names_to_show.iter().enumerate() {
        let normalized = names::normalize(requested);
        let package = packages
            .iter()
            .find(|candidate| names::normalize(&candidate.name) == normalized)
            .ok_or_else(|| Error::Cli(format!("package `{requested}` is not installed")))?;
        if index > 0 {
            println!();
        }
        print_show_block(&layout, package, metadata.get(&names::normalize(&package.name)), &metadata);
    }
    Ok(())
}

fn download_command(args: DownloadArgs) -> Result<()> {
    if args.requirements.is_empty() && args.requirement_files.is_empty() {
        return Err(Error::Cli("download requires at least one requirement".to_owned()));
    }
    let manifest_path = args.manifest.manifest;
    let layout = layout_for_manifest(&manifest_path);
    let manifest = PyProject::read(&manifest_path)?;
    let collected = collect_inputs(&args.requirements, &args.requirement_files, &[], &args.constraint_files)?;
    let index_url = args.index.index_url.clone().or(collected.index_url.clone());
    let extra_index_urls = merged_extra_index_urls(&args.index.extra_index_url, &collected.extra_index_urls);
    let no_index = args.no_index || collected.no_index;
    let allow_prerelease = args.pre || collected.pre || manifest.tool_pon_allow_prerelease();
    let options = ResolveOptions {
        allow_prerelease,
        no_deps: args.no_deps,
        constraints: collect_constraints(&collected.constraints)?,
    };
    let index = selected_index(&manifest_path, &layout, index_url.as_deref(), &extra_index_urls, no_index)?;
    let hash_mode = args.require_hashes || collected.require_hashes || collected.requirements.iter().any(|req| !req.hashes.is_empty());
    let dependencies = resolve_requirement_specs(&index, &collected.requirements, &options, &layout.project_root)?;
    let hash_requirements = enforce_hash_mode(&collected.requirements, hash_mode, &layout.project_root, &dependencies)?;
    fs::create_dir_all(&args.dest)?;
    for dependency in &dependencies {
        download_resolved_artifact(dependency, &index, &args.dest, hash_requirements.as_ref())?;
    }
    Ok(())
}

fn check_command() -> Result<()> {
    let layout = discover_layout()?;
    let packages = read_installed_packages(&layout)?;
    let installed = packages
        .iter()
        .map(|package| (names::normalize(&package.name), package))
        .collect::<HashMap<_, _>>();
    let marker_env = pon_marker_env();
    let mut violations = Vec::new();
    for package in &packages {
        let Some(metadata) = read_installed_core_metadata(&layout, package)? else {
            continue;
        };
        for requirement in metadata.requires_dist {
            if !requirement.evaluate_markers(&marker_env, &[]) {
                continue;
            }
            let required_name = names::normalize(requirement.name.as_ref());
            let Some(installed_package) = installed.get(&required_name) else {
                violations.push(format!(
                    "package `{}` requires `{}` but it is not installed",
                    package.name, requirement
                ));
                continue;
            };
            if !installed_satisfies_requirement(installed_package, &requirement)? {
                violations.push(format!(
                    "package `{}` requires `{}` but `{} {}` is installed",
                    package.name, requirement, installed_package.name, installed_package.version
                ));
            }
        }
    }

    if violations.is_empty() {
        Ok(())
    } else {
        for violation in &violations {
            println!("{violation}");
        }
        Err(Error::Cli("dependency check failed".to_owned()))
    }
}

fn cache_command(args: CacheArgs) -> Result<()> {
    let layout = discover_layout()?;
    let cache = cache_root(&layout);
    match args.action {
        CacheAction::Dir => {
            println!("{}", cache.display());
            Ok(())
        }
        CacheAction::Purge => {
            for subdir in ["http", "wheels", "built", "git"] {
                let path = cache.join(subdir);
                match fs::remove_dir_all(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
            Ok(())
        }
    }
}

fn env_command(root: Option<PathBuf>) -> Result<()> {
    let layout = EnvLayout::new(root.unwrap_or_else(|| PathBuf::from(".")));
    println!("PON_HOME={}", layout.pon_dir.display());
    println!("PONPATH={}", layout.import_path_string());
    println!("PON_IMPORT_PATH={}", layout.import_path_string());
    println!("PON_NATIVE_MODULE_REGISTRY={}", layout.native_registry_path.display());
    Ok(())
}

fn resolve_manifest(manifest: &PyProject, index: &impl PackageIndex, options: &ResolveOptions) -> Result<Vec<ResolvedDist>> {
    let requirements = manifest_requirement_specs(manifest)?;
    resolve_requirement_specs(index, &requirements, options, &manifest_base_dir(manifest))
}

fn resolve_requirement_specs(
    index: &impl PackageIndex,
    requirements: &[RequirementSpec],
    options: &ResolveOptions,
    project_root: &Path,
) -> Result<Vec<ResolvedDist>> {
    let source = IndexSource::new(index);
    let raw_requirements = requirements.iter().map(|requirement| requirement.raw.as_str());
    let provider = PonProvider::from_requirements(&source, raw_requirements)?
        .with_allow_prerelease(options.allow_prerelease)
        .with_no_deps(options.no_deps)
        .with_constraints(options.constraints.clone());
    let mut resolution = resolve_root(&provider)?.dists;
    apply_editable_overrides(&mut resolution, requirements, project_root)?;
    for dependency in &resolution {
        reject_cabi_dist(dependency)?;
    }
    Ok(resolution)
}

fn collect_inputs(
    raw_requirements: &[String],
    requirement_files: &[PathBuf],
    editables: &[PathBuf],
    constraint_files: &[PathBuf],
) -> Result<CollectedInputs> {
    let mut collected = CollectedInputs::default();
    collected.constraints.extend(constraint_files.iter().cloned());
    for raw in raw_requirements {
        collected.requirements.push(requirement_spec_from_cli(raw, false, Path::new("."))?);
    }
    for path in editables {
        let raw = path.display().to_string();
        collected.requirements.push(requirement_spec_from_cli(&raw, true, Path::new("."))?);
    }
    for path in requirement_files {
        merge_requirements_file(&mut collected, parse_requirements_file(path)?)?;
    }
    Ok(collected)
}

fn merge_requirements_file(collected: &mut CollectedInputs, file: RequirementsFile) -> Result<()> {
    collected.requirements.extend(
        file.entries
            .into_iter()
            .map(requirement_spec_from_entry)
            .collect::<Result<Vec<_>>>()?,
    );
    collected.constraints.extend(file.constraints);
    if file.index_url.is_some() {
        collected.index_url = file.index_url;
    }
    collected.extra_index_urls.extend(file.extra_index_urls);
    collected.no_index |= file.no_index;
    collected.pre |= file.pre;
    collected.require_hashes |= file.require_hashes;
    Ok(())
}

fn requirement_spec_from_cli(raw: &str, editable: bool, project_root: &Path) -> Result<RequirementSpec> {
    let mut input = parse_requirement_input(raw)?;
    if editable {
        if !input.set_editable(true) {
            return Err(Error::Cli("editable requirements must be local directories".to_owned()));
        }
        let Some((path, _)) = input.as_path() else {
            return Err(Error::Cli("editable requirements must be local directories".to_owned()));
        };
        let resolved = if path.is_absolute() { path.to_path_buf() } else { project_root.join(path) };
        if !resolved.is_dir() {
            return Err(Error::Cli("editable requirements must be local directories".to_owned()));
        }
        input = RequirementInput::Path {
            path: resolved,
            editable: true,
        };
    }
    Ok(RequirementSpec {
        raw: requirement_input_to_raw(&input),
        input,
        hashes: Vec::new(),
    })
}

fn requirement_spec_from_entry(entry: RequirementEntry) -> Result<RequirementSpec> {
    Ok(RequirementSpec {
        raw: requirement_input_to_raw(&entry.input),
        input: entry.input,
        hashes: entry.hashes,
    })
}

fn manifest_requirement_specs(manifest: &PyProject) -> Result<Vec<RequirementSpec>> {
    let sources = manifest.sources();
    let base_dir = manifest_base_dir(manifest);
    manifest
        .dependencies()
        .into_iter()
        .map(|raw| manifest_requirement_spec(&raw, &sources, &base_dir))
        .collect()
}

fn manifest_requirement_spec(raw: &str, sources: &BTreeMap<String, PonSource>, base_dir: &Path) -> Result<RequirementSpec> {
    let input = parse_requirement_input(raw)?;
    let name = names::normalize(&normalized_name_of(&input, base_dir)?);
    if let Some(source) = sources.get(&name) {
        let source_input = if let Some(path) = &source.path {
            RequirementInput::Path {
                path: path.clone(),
                editable: source.editable,
            }
        } else if let Some(git) = &source.git {
            parse_requirement_input(&format!("{name} @ {}", git_url_with_rev(git, source.rev.as_deref())))?
        } else {
            return Err(Error::Manifest {
                path: base_dir.join("pyproject.toml"),
                message: format!("[tool.pon.sources].{name} must specify exactly one of `path` or `git`"),
            });
        };
        return Ok(RequirementSpec {
            raw: requirement_input_to_raw(&source_input),
            input: source_input,
            hashes: Vec::new(),
        });
    }

    Ok(RequirementSpec {
        raw: raw.to_owned(),
        input,
        hashes: Vec::new(),
    })
}

fn persist_dependency(manifest: &mut PyProject, requirement: &RequirementSpec, project_root: &Path) -> Result<bool> {
    if let Some((name, source)) = source_for_requirement(requirement, project_root)? {
        let changed = manifest.add_dependency(&name)?;
        manifest.set_source(&name, &source);
        Ok(changed)
    } else {
        manifest.add_dependency(&requirement.raw)
    }
}

fn source_for_requirement(requirement: &RequirementSpec, project_root: &Path) -> Result<Option<(String, PonSource)>> {
    match &requirement.input {
        RequirementInput::Path { path, editable } => {
            let name = normalized_name_of(&requirement.input, project_root)?;
            Ok(Some((
                name,
                PonSource {
                    path: Some(path.clone()),
                    editable: *editable,
                    git: None,
                    rev: None,
                },
            )))
        }
        RequirementInput::Url { url } if is_git_requirement_url(&url.to_string()) => {
            let name = normalized_name_of(&requirement.input, project_root)?;
            Ok(Some((
                name,
                PonSource {
                    path: None,
                    editable: false,
                    git: Some(url.to_string()),
                    rev: None,
                },
            )))
        }
        RequirementInput::Pep508(requirement) => {
            if let Some(VersionOrUrl::Url(url)) = &requirement.version_or_url {
                let url = url.to_string();
                if is_git_requirement_url(&url) {
                    return Ok(Some((
                        requirement.name.as_ref().to_owned(),
                        PonSource {
                            path: None,
                            editable: false,
                            git: Some(url),
                            rev: None,
                        },
                    )));
                }
            }
            Ok(None)
        }
        RequirementInput::Url { .. } => Ok(None),
    }
}

fn collect_constraints(paths: &[PathBuf]) -> Result<HashMap<String, VersionSpecifiers>> {
    let mut constraints = HashMap::new();
    let mut seen = BTreeSet::new();
    for path in paths {
        collect_constraint_file(path, &mut seen, &mut constraints)?;
    }
    Ok(constraints)
}

fn collect_constraint_file(
    path: &Path,
    seen: &mut BTreeSet<PathBuf>,
    constraints: &mut HashMap<String, VersionSpecifiers>,
) -> Result<()> {
    let key = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !seen.insert(key) {
        return Ok(());
    }
    let parsed = parse_requirements_file(path)?;
    for entry in parsed.entries {
        let (name, specifier) = constraint_from_entry(&entry)?;
        merge_constraint(constraints, &name, &specifier)?;
    }
    for nested in parsed.constraints {
        collect_constraint_file(&nested, seen, constraints)?;
    }
    Ok(())
}

fn constraint_from_entry(entry: &RequirementEntry) -> Result<(String, VersionSpecifiers)> {
    let RequirementInput::Pep508(requirement) = &entry.input else {
        return Err(invalid_constraint(entry));
    };
    if !requirement.extras.is_empty() {
        return Err(invalid_constraint(entry));
    }
    let Some(VersionOrUrl::VersionSpecifier(specifier)) = &requirement.version_or_url else {
        return Err(invalid_constraint(entry));
    };
    Ok((names::normalize(requirement.name.as_ref()), specifier.clone()))
}

fn invalid_constraint(entry: &RequirementEntry) -> Error {
    Error::InvalidRequirement(format!(
        "constraints cannot use extras or URLs: {}",
        requirement_input_to_raw(&entry.input)
    ))
}

fn merge_constraint(
    constraints: &mut HashMap<String, VersionSpecifiers>,
    name: &str,
    specifier: &VersionSpecifiers,
) -> Result<()> {
    if let Some(existing) = constraints.get(name) {
        let merged = format!("{existing},{specifier}");
        let parsed = VersionSpecifiers::from_str(&merged).map_err(|_| Error::InvalidSpecifier(merged.clone()))?;
        constraints.insert(name.to_owned(), parsed);
    } else {
        constraints.insert(name.to_owned(), specifier.clone());
    }
    Ok(())
}

fn enforce_hash_mode(
    requirements: &[RequirementSpec],
    hash_mode: bool,
    project_root: &Path,
    dependencies: &[ResolvedDist],
) -> Result<Option<HashMap<String, Vec<String>>>> {
    if !hash_mode {
        return Ok(None);
    }

    let mut hashes = HashMap::<String, Vec<String>>::new();
    for requirement in requirements {
        if let Some(kind) = unverifiable_hash_kind(&requirement.input) {
            return Err(Error::Cli(format!(
                "cannot verify hashes for {kind} requirement `{}`",
                requirement.raw
            )));
        }
        if let RequirementInput::Pep508(parsed) = &requirement.input {
            if is_registry_requirement(parsed) && !is_exact_equal_requirement(parsed) {
                return Err(Error::Cli(format!(
                    "in --require-hashes mode, all requirements must be pinned with ==: `{}`",
                    requirement.raw
                )));
            }
        }
        if requirement.hashes.is_empty() {
            return Err(Error::Cli(format!(
                "in --require-hashes mode, all requirements must have a --hash: `{}`",
                requirement.raw
            )));
        }
        let name = names::normalize(&normalized_name_of(&requirement.input, project_root)?);
        hashes.entry(name).or_default().extend(requirement.hashes.iter().cloned());
    }

    for dependency in dependencies {
        let name = names::normalize(&dependency.name);
        if !hashes.contains_key(&name) {
            return Err(Error::Cli(format!(
                "in --require-hashes mode, all transitive dependencies must be listed: missing `{name}`"
            )));
        }
    }

    Ok(Some(hashes))
}

fn unverifiable_hash_kind(input: &RequirementInput) -> Option<&'static str> {
    match input {
        RequirementInput::Path { path, editable } => {
            if *editable {
                Some("editable")
            } else if path.is_dir() {
                Some("local directory")
            } else {
                None
            }
        }
        RequirementInput::Url { url } if is_git_requirement_url(&url.to_string()) => Some("VCS"),
        RequirementInput::Pep508(requirement) => match &requirement.version_or_url {
            Some(VersionOrUrl::Url(url)) if is_git_requirement_url(&url.to_string()) => Some("VCS"),
            _ => None,
        },
        RequirementInput::Url { .. } => None,
    }
}

fn is_registry_requirement(requirement: &Requirement) -> bool {
    !matches!(&requirement.version_or_url, Some(VersionOrUrl::Url(_)))
}

fn is_exact_equal_requirement(requirement: &Requirement) -> bool {
    let Some(VersionOrUrl::VersionSpecifier(specifiers)) = &requirement.version_or_url else {
        return false;
    };
    specifiers.len() == 1 && specifiers[0].operator() == &Operator::Equal && !specifiers.to_string().contains('*')
}

fn install_dependencies(
    layout: &EnvLayout,
    dependencies: &[ResolvedDist],
    index: &impl PackageIndex,
    hashes: Option<&HashMap<String, Vec<String>>>,
) -> Result<()> {
    for dependency in dependencies {
        let install_record = install_record_for(layout, dependency, index, hashes)?;
        install_package(layout, &install_record)?;
    }
    Ok(())
}

fn install_record_for(
    layout: &EnvLayout,
    dist: &ResolvedDist,
    index: &impl PackageIndex,
    hashes: Option<&HashMap<String, Vec<String>>>,
) -> Result<ResolvedRecord> {
    if dist.kind.is_refused() {
        return Err(cabi_error(&package_record_from_dist(dist)));
    }

    let version = dist.version.to_string();
    match &dist.artifact {
        ResolvedArtifact::Wheel(file) => {
            let path = index.fetch_artifact(file)?;
            verify_hash_for_dist(dist, &path, hashes)?;
            Ok(ResolvedRecord::wheel(dist.name.clone(), version, path))
        }
        ResolvedArtifact::Sdist(file) => {
            let path = index.fetch_artifact(file)?;
            verify_hash_for_dist(dist, &path, hashes)?;
            let filename = path.to_string_lossy();
            let sdist_record = ResolvedRecord::sdist(dist.name.clone(), version, &path);
            crate::sdist::build_sdist_with_index(layout, &sdist_record, &filename, index)
        }
        ResolvedArtifact::Dir { path, editable } => Ok(ResolvedRecord::dir(dist.name.clone(), version, path.clone(), *editable)),
        ResolvedArtifact::Vcs { dir, .. } => Ok(ResolvedRecord::dir(dist.name.clone(), version, dir.clone(), false)),
    }
}

fn install_locked_packages(layout: &EnvLayout, lock: &LockFile) -> Result<usize> {
    for package in &lock.packages {
        if package.kind.tool_kind() == Some("cabi-refused") {
            let record = PackageRecord {
                name: package.name.clone(),
                version: package.version.clone(),
                kind: PackageKind::CAbiRefused {
                    reason: "locked package is marked cabi-refused".to_owned(),
                },
            };
            return Err(cabi_error(&record));
        }
        let record = locked_package_record(layout, package)?;
        install_package(layout, &record)?;
    }
    Ok(lock.packages.len())
}

fn locked_package_record(layout: &EnvLayout, package: &crate::lock::LockedPackage) -> Result<ResolvedRecord> {
    if let Some(wheel) = package.wheels.first() {
        let path = download_locked_artifact(layout, &wheel.name, &wheel.url, &wheel.hashes)?;
        return Ok(ResolvedRecord::wheel(package.name.clone(), package.version.clone(), path));
    }
    if let Some(sdist) = &package.sdist {
        let path = download_locked_artifact(layout, &sdist.name, &sdist.url, &sdist.hashes)?;
        return Ok(ResolvedRecord::sdist(package.name.clone(), package.version.clone(), path));
    }
    if let Some(directory) = &package.directory {
        let path = if directory.path.is_absolute() {
            directory.path.clone()
        } else {
            layout.project_root.join(&directory.path)
        };
        return Ok(ResolvedRecord::dir(
            package.name.clone(),
            package.version.clone(),
            path,
            directory.editable,
        ));
    }
    if let Some(vcs) = &package.vcs {
        if vcs.vcs_type != "git" {
            return Err(Error::InvalidRequirement(format!(
                "unsupported VCS scheme `{}`; only git is supported",
                vcs.vcs_type
            )));
        }
        let checkout = crate::vcs::fetch_git(&cache_root(layout), &vcs.url, vcs.requested_revision.as_deref())?;
        return Ok(ResolvedRecord::dir(
            package.name.clone(),
            package.version.clone(),
            checkout.dir,
            false,
        ));
    }
    Err(Error::Cli(format!(
        "pon.lock package `{}` has no installable artifact",
        package.name
    )))
}

fn download_locked_artifact(
    layout: &EnvLayout,
    filename: &str,
    url: &str,
    hashes: &BTreeMap<String, String>,
) -> Result<PathBuf> {
    let required = hash_entries_from_lock(hashes);
    if required.is_empty() {
        download_artifact(&cache_root(layout), url, filename, &HashPolicy::Index(hashes))
    } else {
        download_artifact(&cache_root(layout), url, filename, &HashPolicy::Required(&required))
    }
}

fn download_resolved_artifact(
    dist: &ResolvedDist,
    index: &impl PackageIndex,
    dest: &Path,
    hashes: Option<&HashMap<String, Vec<String>>>,
) -> Result<()> {
    let (source, filename) = match &dist.artifact {
        ResolvedArtifact::Wheel(file) | ResolvedArtifact::Sdist(file) => {
            let source = index.fetch_artifact(file)?;
            verify_hash_for_dist(dist, &source, hashes)?;
            let filename = Path::new(&file.filename)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&file.filename)
                .to_owned();
            (source, filename)
        }
        ResolvedArtifact::Dir { .. } => {
            return Err(Error::Cli(format!(
                "cannot download directory requirement `{}`",
                dist.name
            )));
        }
        ResolvedArtifact::Vcs { .. } => {
            return Err(Error::Cli(format!("cannot download VCS requirement `{}`", dist.name)));
        }
    };
    fs::copy(source, dest.join(filename))?;
    Ok(())
}

fn verify_hash_for_dist(dist: &ResolvedDist, path: &Path, hashes: Option<&HashMap<String, Vec<String>>>) -> Result<()> {
    let Some(hashes) = hashes else {
        return Ok(());
    };
    let name = names::normalize(&dist.name);
    let Some(required) = hashes.get(&name) else {
        return Err(Error::Cli(format!(
            "in --require-hashes mode, all transitive dependencies must be listed: missing `{name}`"
        )));
    };
    verify_required_hashes(path, artifact_label(path), required)
}

fn verify_required_hashes(path: &Path, filename: &str, required: &[String]) -> Result<()> {
    let actual = sha256_file(path)?;
    let mut expected = Vec::with_capacity(required.len());
    for entry in required {
        let (algorithm, hash) = entry.split_once(':').unwrap_or((entry.as_str(), ""));
        if !algorithm.eq_ignore_ascii_case("sha256") {
            return Err(Error::Index(format!(
                "unsupported hash algorithm `{algorithm}`; only sha256 is supported"
            )));
        }
        expected.push(hash);
    }
    if expected.iter().any(|hash| hash.eq_ignore_ascii_case(&actual)) {
        Ok(())
    } else {
        Err(Error::Index(format!(
            "hash mismatch for `{filename}`: expected {}, got sha256:{actual}",
            required.join(", ")
        )))
    }
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn artifact_label(path: &Path) -> &str {
    path.file_name().and_then(|name| name.to_str()).unwrap_or("artifact")
}

fn hash_entries_from_lock(hashes: &BTreeMap<String, String>) -> Vec<String> {
    hashes
        .iter()
        .map(|(algorithm, hash)| format!("{algorithm}:{hash}"))
        .collect()
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

fn write_lock(layout: &EnvLayout, dependencies: &[ResolvedDist], input_hash: &str) -> Result<()> {
    fs::create_dir_all(&layout.project_root)?;
    let resolution = Resolution {
        dists: dependencies.to_vec(),
    };
    LockFile::from_resolution(&resolution, DEFAULT_REQUIRES_PYTHON, input_hash)
        .write_to_path(layout.project_root.join("pon.lock"))
}

fn manifest_input_hash(
    manifest: &PyProject,
    manifest_path: &Path,
    layout: &EnvLayout,
    index_url: Option<&str>,
    no_index: bool,
    allow_prerelease: bool,
) -> Result<String> {
    let effective = effective_index_url(manifest_path, layout, index_url, no_index)?;
    Ok(compute_input_hash(
        manifest.dependencies().iter().map(String::as_str),
        Some(effective.as_str()),
        allow_prerelease,
    ))
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

fn selected_index(
    manifest_path: &Path,
    layout: &EnvLayout,
    index_url: Option<&str>,
    extra_index_urls: &[String],
    no_index: bool,
) -> Result<SelectedIndex> {
    let base_url = effective_index_url(manifest_path, layout, index_url, no_index)?;
    if base_url == "catalog:" {
        Ok(SelectedIndex::catalog())
    } else {
        let mut index_urls = Vec::with_capacity(extra_index_urls.len() + 1);
        index_urls.push(base_url);
        index_urls.extend(extra_index_urls.iter().cloned());
        Ok(SelectedIndex::simple_json(index_urls, pon_home(layout)))
    }
}

fn effective_index_url(manifest_path: &Path, _layout: &EnvLayout, index_url: Option<&str>, no_index: bool) -> Result<String> {
    if no_index {
        return Ok("catalog:".to_owned());
    }
    if let Some(url) = index_url.filter(|url| !url.trim().is_empty()) {
        return Ok(url.to_owned());
    }
    if let Ok(url) = env::var("PON_INDEX_URL") {
        if !url.trim().is_empty() {
            return Ok(url);
        }
    }
    Ok(PyProject::read(manifest_path)?
        .tool_pon_index_url()
        .map_or_else(|| DEFAULT_INDEX_URL.to_owned(), str::to_owned))
}

fn pon_home(layout: &EnvLayout) -> PathBuf {
    env::var_os("PON_HOME").map_or_else(|| layout.pon_dir.clone(), PathBuf::from)
}

fn cache_root(layout: &EnvLayout) -> PathBuf {
    pon_home(layout).join("cache")
}

fn merged_extra_index_urls(cli: &[String], files: &[String]) -> Vec<String> {
    let mut merged = Vec::with_capacity(cli.len() + files.len());
    merged.extend(cli.iter().cloned());
    merged.extend(files.iter().cloned());
    merged
}

fn manifest_base_dir(manifest: &PyProject) -> PathBuf {
    manifest
        .path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

fn requirement_input_to_raw(input: &RequirementInput) -> String {
    match input {
        RequirementInput::Pep508(requirement) => requirement.to_string(),
        RequirementInput::Path { path, .. } => path.display().to_string(),
        RequirementInput::Url { url } => url.to_string(),
    }
}

fn apply_editable_overrides(
    dependencies: &mut [ResolvedDist],
    requirements: &[RequirementSpec],
    project_root: &Path,
) -> Result<()> {
    let mut editables = HashMap::<String, PathBuf>::new();
    for requirement in requirements {
        if let RequirementInput::Path { path, editable: true } = &requirement.input {
            let name = names::normalize(&normalized_name_of(&requirement.input, project_root)?);
            editables.insert(name, path.clone());
        }
    }
    for dependency in dependencies {
        if let Some(path) = editables.get(&names::normalize(&dependency.name)) {
            dependency.artifact = ResolvedArtifact::Dir {
                path: path.clone(),
                editable: true,
            };
        }
    }
    Ok(())
}

fn print_dry_run(dependencies: &[ResolvedDist]) {
    for dependency in dependencies {
        println!("would install {} {}", dependency.name, dependency.version);
    }
}

fn print_show_block(
    layout: &EnvLayout,
    package: &InstalledPackageRecord,
    metadata: Option<&CoreMetadata>,
    all_metadata: &HashMap<String, CoreMetadata>,
) {
    let requires = metadata.map_or_else(String::new, |metadata| {
        metadata
            .requires_dist
            .iter()
            .map(|requirement| requirement.name.as_ref().to_owned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(", ")
    });
    let required_by = required_by(package, all_metadata).join(", ");
    println!("Name: {}", metadata.map_or(package.name.as_str(), |metadata| metadata.name.as_str()));
    println!(
        "Version: {}",
        metadata.map_or_else(|| package.version.clone(), |metadata| metadata.version.to_string())
    );
    println!("Summary: {}", metadata.and_then(|metadata| metadata.summary.as_deref()).unwrap_or(""));
    println!("Home-page: {}", metadata.and_then(|metadata| metadata.home_page.as_deref()).unwrap_or(""));
    println!("Author: {}", metadata.and_then(|metadata| metadata.author.as_deref()).unwrap_or(""));
    println!(
        "Author-email: {}",
        metadata.and_then(|metadata| metadata.author_email.as_deref()).unwrap_or("")
    );
    println!("License: {}", metadata.and_then(|metadata| metadata.license.as_deref()).unwrap_or(""));
    println!("Location: {}", layout.site_packages.display());
    println!("Requires: {requires}");
    println!("Required-by: {required_by}");
}

fn installed_metadata_map(
    layout: &EnvLayout,
    packages: &[InstalledPackageRecord],
) -> Result<HashMap<String, CoreMetadata>> {
    let mut metadata = HashMap::new();
    for package in packages {
        if let Some(core_metadata) = read_installed_core_metadata(layout, package)? {
            metadata.insert(names::normalize(&package.name), core_metadata);
        }
    }
    Ok(metadata)
}

fn read_installed_core_metadata(layout: &EnvLayout, package: &InstalledPackageRecord) -> Result<Option<CoreMetadata>> {
    let Some(text) = read_installed_metadata_text(layout, package)? else {
        return Ok(None);
    };
    parse_core_metadata(&text, &package.name).map(Some)
}

fn read_installed_metadata_text(layout: &EnvLayout, package: &InstalledPackageRecord) -> Result<Option<String>> {
    if let Some(record_path) = &package.record_path {
        if let Some(parent) = record_path.parent() {
            let metadata_path = layout.site_packages.join(parent).join("METADATA");
            if metadata_path.is_file() {
                return fs::read_to_string(metadata_path).map(Some).map_err(Into::into);
            }
        }
    }
    let Some(dist_info) = find_dist_info_dir(layout, package)? else {
        return Ok(None);
    };
    let metadata_path = dist_info.join("METADATA");
    if metadata_path.is_file() {
        fs::read_to_string(metadata_path).map(Some).map_err(Into::into)
    } else {
        Ok(None)
    }
}

fn find_dist_info_dir(layout: &EnvLayout, package: &InstalledPackageRecord) -> Result<Option<PathBuf>> {
    let entries = match fs::read_dir(&layout.site_packages) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let normalized = names::normalize(&package.name);
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".dist-info") else {
            continue;
        };
        let Some((dist, _version)) = stem.rsplit_once('-') else {
            continue;
        };
        if names::normalize(dist) == normalized {
            return Ok(Some(entry.path()));
        }
    }
    Ok(None)
}

fn required_by(package: &InstalledPackageRecord, metadata: &HashMap<String, CoreMetadata>) -> Vec<String> {
    let normalized = names::normalize(&package.name);
    let mut required_by = metadata
        .values()
        .filter(|candidate| names::normalize(&candidate.name) != normalized)
        .filter(|candidate| {
            candidate
                .requires_dist
                .iter()
                .any(|requirement| names::normalize(requirement.name.as_ref()) == normalized)
        })
        .map(|candidate| candidate.name.clone())
        .collect::<Vec<_>>();
    required_by.sort();
    required_by
}

fn installed_satisfies_requirement(package: &InstalledPackageRecord, requirement: &Requirement) -> Result<bool> {
    let version = Version::from_str(&package.version).map_err(|_| Error::InvalidRequirement(package.version.clone()))?;
    match &requirement.version_or_url {
        Some(VersionOrUrl::VersionSpecifier(specifiers)) => Ok(specifiers.contains(&version)),
        Some(VersionOrUrl::Url(_)) | None => Ok(true),
    }
}

fn git_url_with_rev(url: &str, rev: Option<&str>) -> String {
    let Some(rev) = rev else {
        return url.to_owned();
    };
    if url.contains('@') {
        return url.to_owned();
    }
    match url.split_once('#') {
        Some((base, fragment)) => format!("{base}@{rev}#{fragment}"),
        None => format!("{url}@{rev}"),
    }
}

fn is_git_requirement_url(url: &str) -> bool {
    url.trim_start().starts_with("git+")
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
    fn clap_accepts_manifest_and_index_urls_for_resolving_commands() {
        let cli = Cli::try_parse_from([
            "pon-pm",
            "install",
            "--manifest",
            "demo.toml",
            "--index-url",
            "https://packages.example/simple/",
            "--extra-index-url",
            "https://mirror-one.example/simple/",
            "--extra-index-url",
            "https://mirror-two.example/simple/",
        ])
        .expect("parse");

        let Command::Install(options) = cli.command else {
            panic!("install command");
        };
        assert_eq!(options.manifest.manifest, PathBuf::from("demo.toml"));
        assert_eq!(options.index.index_url.as_deref(), Some("https://packages.example/simple/"));
        assert_eq!(
            options.index.extra_index_url,
            vec![
                "https://mirror-one.example/simple/".to_owned(),
                "https://mirror-two.example/simple/".to_owned()
            ]
        );
    }

    #[test]
    fn clap_accepts_install_requirement_file_and_flags() {
        let cli = Cli::try_parse_from([
            "pon-pm",
            "install",
            "requests==2.32.0",
            "-r",
            "requirements.txt",
            "-c",
            "constraints.txt",
            "-e",
            "./pkg",
            "--no-deps",
            "--pre",
            "--require-hashes",
            "--dry-run",
            "--frozen",
            "--no-index",
        ])
        .expect("parse");

        let Command::Install(options) = cli.command else {
            panic!("install command");
        };
        assert_eq!(options.requirements, vec!["requests==2.32.0"]);
        assert_eq!(options.requirement_files, vec![PathBuf::from("requirements.txt")]);
        assert_eq!(options.constraint_files, vec![PathBuf::from("constraints.txt")]);
        assert_eq!(options.editables, vec![PathBuf::from("./pkg")]);
        assert!(options.no_deps);
        assert!(options.pre);
        assert!(options.require_hashes);
        assert!(options.dry_run);
        assert!(options.frozen);
        assert!(options.no_index);
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
    fn add_local_sdist_records_source_and_installs_package() {
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
        assert_eq!(dependencies, vec!["pon-flit-fixture".to_owned()]);
        let sources = pyproject.sources();
        let source = sources.get("pon-flit-fixture").expect("source");
        assert_eq!(source.path.as_ref(), Some(&sdist_path));

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
