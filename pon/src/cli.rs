use std::{
	collections::{BTreeMap, BTreeSet, HashMap},
	env,
	ffi::OsString,
	fs,
	io::{self, IsTerminal, Read},
	path::{Component, Path, PathBuf},
	str::FromStr,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use anyhow::{Context, anyhow};
use clap::{Args, CommandFactory, Parser, Subcommand};
use pep440_rs::{Operator, Version, VersionSpecifiers};
use pep508_rs::{Requirement, VersionOrUrl};
use sha2::{Digest, Sha256};

use crate::{
	env::EnvLayout,
	error::{Error, Result},
	index::{DEFAULT_INDEX_URL, PackageIndex, SelectedIndex},
	install::{
		InstalledPackageRecord, ResolvedRecord, install_package, read_installed_packages,
		remove_installed_package,
	},
	lock::{
		DEFAULT_REQUIRES_PYTHON, LockFile, compute_input_hash_with_entries,
		missing_frozen_lock_error, stale_lock_error,
	},
	marker::pon_marker_env,
	metadata::{CoreMetadata, parse_core_metadata},
	names,
	pyproject::{PonSource, PyProject},
	requirement::{RequirementInput, normalized_name_of, parse_requirement_input},
	requirements::{RequirementEntry, RequirementsFile, parse_requirements_file},
	resolve::{
		Resolution, ResolvedArtifact, ResolvedDist,
		provider::{PonProvider, ResolveProvider},
		resolve_root,
		source::{IndexSource, PackageKind, PackageRecord},
	},
	wheel::record::parse_record,
};

#[derive(Debug, Parser)]
#[command(name = "pon", version, disable_help_subcommand = true)]
struct Cli {
	#[command(subcommand)]
	command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
	/// Run a Python file, inline code, or entry point in the project environment
	Run(RunArgs),
	/// Start the interactive session
	Repl,
	/// Compile a Python file to a native executable
	Build(BuildArgs),
	/// Create a pyproject.toml
	Init(ManifestArgs),
	/// Add dependencies to pyproject.toml and install them
	Add(AddArgs),
	/// Remove dependencies from pyproject.toml
	Remove(RemoveArgs),
	/// Install project dependencies or explicit requirements
	Install(InstallArgs),
	/// Resolve dependencies and write pon.lock
	Lock(LockArgs),
	/// List installed packages
	List(ManifestArgs),
	/// Print installed packages as pinned requirements
	Freeze,
	/// Show metadata for installed packages
	Show(ShowArgs),
	/// Download distribution artifacts without installing
	Download(DownloadArgs),
	/// Verify installed packages satisfy their requirements
	Check,
	/// Inspect or clear the artifact cache
	Cache(CacheArgs),
	/// Print the environment layout
	Env(EnvArgs),
}

#[derive(Clone, Debug, Args)]
struct ManifestArgs {
	#[arg(
		long,
		alias = "pyproject",
		value_name = "pyproject.toml",
		default_value = "pyproject.toml"
	)]
	manifest: PathBuf,
}

#[derive(Clone, Debug, Args, Default)]
struct IndexArgs {
	#[arg(long, value_name = "url")]
	index_url:       Option<String>,
	#[arg(long, value_name = "url")]
	extra_index_url: Vec<String>,
	#[arg(short = 'f', long = "find-links", value_name = "path")]
	find_links:      Vec<PathBuf>,
}

#[derive(Debug, Args)]
struct AddArgs {
	#[arg(value_name = "req", required = true)]
	requirements: Vec<String>,
	#[arg(long)]
	editable:     bool,
	#[arg(long)]
	pre:          bool,
	#[arg(long)]
	allow_unhashed: bool,
	#[command(flatten)]
	manifest:     ManifestArgs,
	#[command(flatten)]
	index:        IndexArgs,
}

#[derive(Debug, Args)]
struct RemoveArgs {
	#[arg(value_name = "name", required = true)]
	names:    Vec<String>,
	#[command(flatten)]
	manifest: ManifestArgs,
}

#[derive(Debug, Args)]
struct InstallArgs {
	#[arg(value_name = "req")]
	requirements:      Vec<String>,
	#[arg(short = 'r', long = "requirement", value_name = "file")]
	requirement_files: Vec<PathBuf>,
	#[arg(short = 'c', long = "constraint", value_name = "file")]
	constraint_files:  Vec<PathBuf>,
	#[arg(short = 'e', long = "editable", value_name = "path")]
	editables:         Vec<PathBuf>,
	#[arg(long)]
	no_deps:           bool,
	#[arg(long)]
	pre:               bool,
	#[arg(long)]
	require_hashes:    bool,
	#[arg(long)]
	dry_run:           bool,
	#[arg(long)]
	frozen:            bool,
	#[arg(long)]
	no_index:          bool,
	#[arg(long)]
	allow_unhashed:    bool,
	#[command(flatten)]
	manifest:          ManifestArgs,
	#[command(flatten)]
	index:             IndexArgs,
}

#[derive(Debug, Args)]
struct LockArgs {
	#[command(flatten)]
	manifest: ManifestArgs,
	#[command(flatten)]
	index:    IndexArgs,
	#[arg(long)]
	allow_unhashed: bool,
}

#[derive(Debug, Args)]
struct RunArgs {
	#[arg(short = 'c', value_name = "code")]
	code:  Option<String>,
	#[arg(long, value_name = "module:attr")]
	entry: Option<String>,
	#[arg(value_name = "args", trailing_var_arg = true, allow_hyphen_values = true)]
	args:  Vec<String>,
}

#[derive(Debug, Args)]
struct BuildArgs {
	/// Python source file to compile
	#[arg(value_name = "file")]
	file:          PathBuf,
	/// Output executable path
	#[arg(short = 'o', long = "output", value_name = "out")]
	out:           PathBuf,
	/// Permit dynamic constructs the AoT backend rejects by default
	#[arg(long)]
	allow_dynamic: bool,
	/// Enable optimization
	#[arg(long)]
	opt:           bool,
	/// Cross-compilation target triple
	#[arg(long, value_name = "triple")]
	target:        Option<String>,
}

#[derive(Debug, Args)]
struct ShowArgs {
	#[arg(value_name = "name", required = true)]
	names: Vec<String>,
}

#[derive(Debug, Args)]
struct DownloadArgs {
	#[arg(value_name = "req")]
	requirements:      Vec<String>,
	#[arg(short = 'd', long = "dest", value_name = "dir")]
	dest:              PathBuf,
	#[arg(short = 'r', long = "requirement", value_name = "file")]
	requirement_files: Vec<PathBuf>,
	#[arg(short = 'c', long = "constraint", value_name = "file")]
	constraint_files:  Vec<PathBuf>,
	#[arg(long)]
	pre:               bool,
	#[arg(long)]
	require_hashes:    bool,
	#[arg(long)]
	no_deps:           bool,
	#[arg(long)]
	no_index:          bool,
	#[arg(long)]
	allow_unhashed:    bool,
	#[command(flatten)]
	manifest:          ManifestArgs,
	#[command(flatten)]
	index:             IndexArgs,
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
struct InputRequirement {
	raw:    String,
	input:  RequirementInput,
	hashes: Vec<String>,
}

#[derive(Clone, Debug, Default)]
struct CollectedInputs {
	requirements:     Vec<InputRequirement>,
	constraints:      Vec<PathBuf>,
	index_url:        Option<String>,
	extra_index_urls: Vec<String>,
	find_links:       Vec<PathBuf>,
	no_index:         bool,
	pre:              bool,
	require_hashes:   bool,
}

#[derive(Clone, Debug, Default)]
struct ResolveOptions {
	allow_prerelease: bool,
	no_deps:          bool,
	constraints:      HashMap<String, VersionSpecifiers>,
}

pub fn run_from_env() -> anyhow::Result<()> {
	run_from_args(env::args())
}

pub fn run_from_args(args: impl IntoIterator<Item = String>) -> anyhow::Result<()> {
	let argv: Vec<String> = args.into_iter().collect();
	match argv.get(1).map(String::as_str) {
		None | Some("-h" | "--help" | "help") => {
			print_root_help();
			Ok(())
		},
		Some("-m") => {
			let module = argv
				.get(2)
				.context("missing module name for `pon -m <module>`")?;
			crate::run::run_module_as_main(module, argv[3..].to_vec())
		},
		Some("-c") => {
			let code = argv.get(2).context("missing code for `pon -c <code>`")?;
			let mut inline_argv = vec!["-c".to_owned()];
			inline_argv.extend(argv[3..].iter().cloned());
			crate::run::run_inline_source(
				code,
				std::iter::empty::<(OsString, OsString)>(),
				&inline_argv,
			)
		},
		Some("-") => {
			let mut code = String::new();
			io::stdin()
				.read_to_string(&mut code)
				.context("failed to read program from stdin")?;
			let mut inline_argv = vec!["-".to_owned()];
			inline_argv.extend(argv[2..].iter().cloned());
			crate::run::run_inline_source(
				&code,
				std::iter::empty::<(OsString, OsString)>(),
				&inline_argv,
			)
		},
		Some(token)
			if !token.starts_with('-')
				&& Cli::command().find_subcommand(token).is_none()
				&& Path::new(token).is_file() =>
		{
			let mut script_argv = vec![token.to_owned()];
			script_argv.extend(argv[2..].iter().cloned());
			crate::run::run_file_with_env(
				token,
				std::iter::empty::<(OsString, OsString)>(),
				&script_argv,
			)
		},
		_ => {
			let cli = Cli::try_parse_from(argv.clone()).unwrap_or_else(|error| error.exit());
			match cli.command {
				Command::Run(args) => Ok(run_command(args)?),
				Command::Repl => crate::repl::run(),
				Command::Build(args) => build_command(args),
				Command::Init(args) => Ok(init_command(args)?),
				Command::Add(args) => Ok(add_command(args)?),
				Command::Remove(args) => Ok(remove_command(args)?),
				Command::Install(args) => Ok(install_command(args)?),
				Command::Lock(args) => Ok(lock_command(args)?),
				Command::List(args) => Ok(list_command(args.manifest.as_path())?),
				Command::Freeze => Ok(freeze_command()?),
				Command::Show(args) => Ok(show_command(&args.names)?),
				Command::Download(args) => Ok(download_command(args)?),
				Command::Check => Ok(check_command()?),
				Command::Cache(args) => Ok(cache_command(args)?),
				Command::Env(args) => Ok(env_command(args.root)?),
			}
		},
	}
}

fn style(on: bool, code: &'static str) -> &'static str {
	if on { code } else { "" }
}

fn print_root_help() {
	let no_color = env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
	let on = io::stdout().is_terminal() && !no_color;
	let bold = style(on, "\x1b[1m");
	let command = style(on, "\x1b[1;36m");
	let dim = style(on, "\x1b[2m");
	let reset = style(on, "\x1b[0m");
	print!(
		"{bold}pon — Python runtime & package manager (v{}){reset}\n\nUsage: pon <command> \
		 [...flags] [...args]\npon <file>.py [args]      run a script (python-style)\npon -m \
		 <module> [args]    run a module as __main__\npon -c <code> [args]      run inline \
		 code\npon -                     run a program from \
		 stdin\n\n{bold}Runtime:{reset}\n{command}run{reset}       {dim}pon run app.py{reset}            \
		 Run a file, -c code, or --entry in the project env\n{command}repl{reset}      {dim}pon \
		 repl{reset}                  Start the interactive session\n{command}build{reset}     \
		 {dim}pon build app.py -o app{reset}   Compile to a native \
		 executable\n\n{bold}Project:{reset}\n{command}init{reset}                                \
		 Create pyproject.toml\n{command}add{reset}       {dim}pon add requests{reset}          Add \
		 dependencies and install them\n{command}remove{reset}    {dim}pon remove requests{reset}       \
		 Remove dependencies\n{command}install{reset}                             Install project \
		 or explicit requirements\n{command}lock{reset}                                Resolve and \
		 write pon.lock\n\n{bold}Inspect:{reset}\n{command}list{reset}                                \
		 List installed packages\n{command}freeze{reset}                              Print pinned \
		 requirements\n{command}show{reset}      {dim}pon show requests{reset}         Show package \
		 metadata\n{command}check{reset}                               Verify installed \
		 packages\n{command}env{reset}                                 Print the environment \
		 layout\n\n{bold}Artifacts:{reset}\n{command}download{reset}                            \
		 Download distribution artifacts\n{command}cache{reset}                               \
		 Inspect or clear the artifact cache\n\n{bold}Flags:{reset}\n-h, --help                          \
		 Show this help (per-command: pon <command> --help)\n-V, --version                       \
		 Print the version\n\n",
		env!("CARGO_PKG_VERSION")
	);
}

fn build_command(args: BuildArgs) -> anyhow::Result<()> {
	let mut options = pon_aot::BuildOptions {
		out_path:      args.out,
		allow_dynamic: args.allow_dynamic,
		opt:           args.opt,
		target:        None,
	};
	if let Some(raw) = args.target {
		options.target = Some(
			raw.parse()
				.map_err(|error| anyhow!("invalid target triple `{raw}`: {error}"))?,
		);
	}
	pon_aot::build(&args.file, &options).map(|_| ())
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
		&args.index.find_links,
		args.allow_unhashed,
	)?;
	let requirements = args
		.requirements
		.iter()
		.map(|raw| requirement_spec_from_cli(raw, args.editable, &layout.project_root))
		.collect::<Result<Vec<_>>>()?;

	let options = ResolveOptions { allow_prerelease, no_deps: false, constraints: HashMap::new() };
	reject_cabi_roots(&index, &requirements, allow_prerelease)?;

	let mut manifest = PyProject::read(&manifest_path)?;
	let mut changed = false;
	for requirement in &requirements {
		changed |= persist_dependency(&mut manifest, requirement, &layout.project_root)?;
	}
	manifest.write()?;

	let dependencies = resolve_manifest(&manifest, &index, &options)?;
	install_dependencies(&layout, &dependencies, &index, None)?;
	let input_hash = manifest_input_hash(
		&manifest,
		&manifest_path,
		&layout,
		args.index.index_url.as_deref(),
		&args.index.extra_index_url,
		false,
		&args.index.find_links,
		&options,
		index_shape(args.index.index_url.as_deref(), false),
	)?;
	write_lock(&layout, &dependencies, &input_hash, &index)?;

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
	let collected = collect_inputs(
		&args.requirements,
		&args.requirement_files,
		&args.editables,
		&args.constraint_files,
	)?;
	let explicit_install = !args.requirements.is_empty()
		|| !args.requirement_files.is_empty()
		|| !args.editables.is_empty();
	let index_url = args.index.index_url.clone().or(collected.index_url.clone());
	let extra_index_urls =
		merged_extra_index_urls(&args.index.extra_index_url, &collected.extra_index_urls);
	let find_links = merged_find_links(&args.index.find_links, &collected.find_links);
	let no_index = args.no_index || collected.no_index;
	let allow_prerelease = args.pre || collected.pre || manifest.tool_pon_allow_prerelease();
	let constraints = collect_constraints(&collected.constraints)?;
	let options = ResolveOptions { allow_prerelease, no_deps: args.no_deps, constraints };
	let index = selected_index(
		&manifest_path,
		&layout,
		index_url.as_deref(),
		&extra_index_urls,
		no_index,
		&find_links,
		args.allow_unhashed,
	)?;
	let hash_mode = args.require_hashes
		|| collected.require_hashes
		|| collected
			.requirements
			.iter()
			.any(|req| !req.hashes.is_empty());

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
			&extra_index_urls,
			no_index,
			&find_links,
			hash_mode || args.require_hashes,
			args.dry_run,
			args.frozen,
		)
	}
}

fn install_explicit_requirements(
	layout: &EnvLayout,
	index: &impl PackageIndex,
	requirements: &[InputRequirement],
	options: &ResolveOptions,
	hash_mode: bool,
	dry_run: bool,
) -> Result<()> {
	let dependencies =
		resolve_requirement_specs(index, requirements, options, &layout.project_root)?;
	let hash_requirements =
		enforce_hash_mode(requirements, hash_mode, &layout.project_root, &dependencies)?;
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
	extra_index_urls: &[String],
	no_index: bool,
	find_links: &[PathBuf],
	hash_mode: bool,
	dry_run: bool,
	frozen: bool,
) -> Result<()> {
	let requirements = manifest_requirement_specs(manifest)?;
	let input_hash = manifest_input_hash(
		manifest,
		manifest_path,
		layout,
		index_url,
		extra_index_urls,
		no_index,
		find_links,
		options,
		index_shape(index_url, no_index),
	)?;
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

	let dependencies =
		resolve_requirement_specs(index, &requirements, options, &layout.project_root)?;
	let hash_requirements =
		enforce_hash_mode(&requirements, hash_mode, &layout.project_root, &dependencies)?;
	if dry_run {
		print_dry_run(&dependencies);
		return Ok(());
	}
	install_dependencies(layout, &dependencies, index, hash_requirements.as_ref())?;
	write_lock(layout, &dependencies, &input_hash, index)?;
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
	let index = selected_index(manifest_path, &layout, None, &[], false, &[], false)?;
	let options = ResolveOptions {
		allow_prerelease: manifest.tool_pon_allow_prerelease(),
		no_deps:          false,
		constraints:      HashMap::new(),
	};
	let dependencies = resolve_manifest(&manifest, &index, &options)?;
	let removed = remove_installed_package(&layout, name)?;
	let input_hash = manifest_input_hash(
		&manifest,
		manifest_path,
		&layout,
		None,
		&[],
		false,
		&[],
		&options,
		index_shape(None, false),
	)?;
	write_lock(&layout, &dependencies, &input_hash, &index)?;
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
		&args.index.find_links,
		args.allow_unhashed,
	)?;
	let options = ResolveOptions {
		allow_prerelease: manifest.tool_pon_allow_prerelease(),
		no_deps:          false,
		constraints:      HashMap::new(),
	};
	let dependencies = resolve_manifest(&manifest, &index, &options)?;
	let input_hash = manifest_input_hash(
		&manifest,
		&manifest_path,
		&layout,
		args.index.index_url.as_deref(),
		&args.index.extra_index_url,
		false,
		&args.index.find_links,
		&options,
		index_shape(args.index.index_url.as_deref(), false),
	)?;
	write_lock(&layout, &dependencies, &input_hash, &index)?;
	println!("wrote {}", layout.project_root.join("pon.lock").display());
	Ok(())
}

fn strip_arg_delimiter(args: &mut Vec<String>, index: usize) {
	if args.get(index).is_some_and(|arg| arg == "--") {
		args.remove(index);
	}
}

fn run_command(mut args: RunArgs) -> Result<()> {
	let extra_env = if let Some(layout) = discover_project()? {
		layout.create_dirs()?;
		runtime_env(&layout)
	} else {
		Vec::new()
	};

	if let Some(code) = args.code {
		if args.entry.is_some() || !args.args.is_empty() {
			return Err(Error::Cli("unexpected argument for `run -c`".to_owned()));
		}
		let argv = [String::from("-c")];
		return crate::run::run_inline_source(&code, extra_env, &argv)
			.map_err(|error| Error::Cli(format!("{error:#}")));
	}

	if let Some(entry) = args.entry {
		strip_arg_delimiter(&mut args.args, 0);
		let (module, attr) = entry
			.split_once(':')
			.filter(|(module, attr)| !module.is_empty() && !attr.is_empty())
			.ok_or_else(|| Error::Cli("entry point must be `<module>:<attr>`".to_owned()))?;
		let code = format!("import {module} as _pon_entry\n_pon_entry.{attr}()\n");
		let mut argv = Vec::with_capacity(args.args.len() + 1);
		argv.push(entry);
		argv.extend(args.args);
		return crate::run::run_inline_source(&code, extra_env, &argv)
			.map_err(|error| Error::Cli(format!("{error:#}")));
	}

	let Some(file) = args.args.first().cloned() else {
		return Err(Error::Cli("missing file or `-c <code>` for `run`".to_owned()));
	};
	let mut argv = args.args;
	strip_arg_delimiter(&mut argv, 1);
	crate::run::run_file_with_env(file.as_str(), extra_env, &argv)
		.map_err(|error| Error::Cli(format!("{error:#}")))
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
	let collected =
		collect_inputs(&args.requirements, &args.requirement_files, &[], &args.constraint_files)?;
	let index_url = args.index.index_url.clone().or(collected.index_url.clone());
	let extra_index_urls =
		merged_extra_index_urls(&args.index.extra_index_url, &collected.extra_index_urls);
	let find_links = merged_find_links(&args.index.find_links, &collected.find_links);
	let no_index = args.no_index || collected.no_index;
	let allow_prerelease = args.pre || collected.pre || manifest.tool_pon_allow_prerelease();
	let options = ResolveOptions {
		allow_prerelease,
		no_deps: args.no_deps,
		constraints: collect_constraints(&collected.constraints)?,
	};
	let index = selected_index(
		&manifest_path,
		&layout,
		index_url.as_deref(),
		&extra_index_urls,
		no_index,
		&find_links,
		args.allow_unhashed,
	)?;
	let hash_mode = args.require_hashes
		|| collected.require_hashes
		|| collected
			.requirements
			.iter()
			.any(|req| !req.hashes.is_empty());
	let dependencies =
		resolve_requirement_specs(&index, &collected.requirements, &options, &layout.project_root)?;
	let hash_requirements =
		enforce_hash_mode(&collected.requirements, hash_mode, &layout.project_root, &dependencies)?;
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
		violations.extend(check_record_integrity(&layout, package)?);
		let Some(metadata) = read_installed_core_metadata(&layout, package)? else {
			violations.push(format!("package `{}` is missing METADATA", package.name));
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
		},
		CacheAction::Purge => {
			for subdir in ["http", "wheels", "built", "git"] {
				let path = cache.join(subdir);
				match fs::remove_dir_all(&path) {
					Ok(()) => {},
					Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
					Err(error) => return Err(error.into()),
				}
			}
			Ok(())
		},
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

fn resolve_manifest(
	manifest: &PyProject,
	index: &impl PackageIndex,
	options: &ResolveOptions,
) -> Result<Vec<ResolvedDist>> {
	let requirements = manifest_requirement_specs(manifest)?;
	resolve_requirement_specs(index, &requirements, options, &manifest_base_dir(manifest))
}

fn resolve_requirement_specs(
	index: &impl PackageIndex,
	requirements: &[InputRequirement],
	options: &ResolveOptions,
	project_root: &Path,
) -> Result<Vec<ResolvedDist>> {
	let source = IndexSource::new(index);
	let raw_requirements = requirements
		.iter()
		.map(|requirement| requirement.raw.as_str());
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
	collected
		.constraints
		.extend(constraint_files.iter().cloned());
	for raw in raw_requirements {
		collected
			.requirements
			.push(requirement_spec_from_cli(raw, false, Path::new("."))?);
	}
	for path in editables {
		let raw = path.display().to_string();
		collected
			.requirements
			.push(requirement_spec_from_cli(&raw, true, Path::new("."))?);
	}
	for path in requirement_files {
		merge_requirements_file(&mut collected, parse_requirements_file(path)?)?;
	}
	Ok(collected)
}

fn merge_requirements_file(collected: &mut CollectedInputs, file: RequirementsFile) -> Result<()> {
	collected.requirements.extend(
		file
			.entries
			.into_iter()
			.map(requirement_spec_from_entry)
			.collect::<Result<Vec<_>>>()?,
	);
	collected.constraints.extend(file.constraints);
	if file.index_url.is_some() {
		collected.index_url = file.index_url;
	}
	collected.extra_index_urls.extend(file.extra_index_urls);
	collected.find_links.extend(file.find_links);
	collected.no_index |= file.no_index;
	collected.pre |= file.pre;
	collected.require_hashes |= file.require_hashes;
	Ok(())
}

fn requirement_spec_from_cli(
	raw: &str,
	editable: bool,
	project_root: &Path,
) -> Result<InputRequirement> {
	let mut input = parse_requirement_input(raw)?;
	if editable {
		if !input.set_editable(true) {
			return Err(Error::Cli("editable requirements must be local directories".to_owned()));
		}
		let Some((path, _)) = input.as_path() else {
			return Err(Error::Cli("editable requirements must be local directories".to_owned()));
		};
		let resolved = if path.is_absolute() {
			path.to_path_buf()
		} else {
			project_root.join(path)
		};
		if !resolved.is_dir() {
			return Err(Error::Cli("editable requirements must be local directories".to_owned()));
		}
		input = RequirementInput::Path { path: resolved, editable: true };
	}
	Ok(InputRequirement { raw: requirement_input_to_raw(&input), input, hashes: Vec::new() })
}

fn requirement_spec_from_entry(entry: RequirementEntry) -> Result<InputRequirement> {
	Ok(InputRequirement {
		raw:    requirement_input_to_raw(&entry.input),
		input:  entry.input,
		hashes: entry.hashes,
	})
}

fn manifest_requirement_specs(manifest: &PyProject) -> Result<Vec<InputRequirement>> {
	let sources = manifest.sources();
	let base_dir = manifest_base_dir(manifest);
	manifest
		.dependencies()
		.into_iter()
		.map(|raw| manifest_requirement_spec(&raw, &sources, &base_dir))
		.collect()
}

fn manifest_requirement_spec(
	raw: &str,
	sources: &BTreeMap<String, PonSource>,
	base_dir: &Path,
) -> Result<InputRequirement> {
	let input = parse_requirement_input(raw)?;
	let name = names::normalize(&normalized_name_of(&input, base_dir)?);
	if let Some(source) = sources.get(&name) {
		let source_input = if let Some(path) = &source.path {
			RequirementInput::Path { path: path.clone(), editable: source.editable }
		} else if let Some(git) = &source.git {
			parse_requirement_input(&format!(
				"{name} @ {}",
				git_url_with_rev(git, source.rev.as_deref())
			))?
		} else {
			return Err(Error::Manifest {
				path:    base_dir.join("pyproject.toml"),
				message: format!(
					"[tool.pon.sources].{name} must specify exactly one of `path` or `git`"
				),
			});
		};
		return Ok(InputRequirement {
			raw:    requirement_input_to_raw(&source_input),
			input:  source_input,
			hashes: Vec::new(),
		});
	}

	Ok(InputRequirement { raw: raw.to_owned(), input, hashes: Vec::new() })
}

fn persist_dependency(
	manifest: &mut PyProject,
	requirement: &InputRequirement,
	project_root: &Path,
) -> Result<bool> {
	if let Some((name, source)) = source_for_requirement(requirement, project_root)? {
		let changed = manifest.add_dependency(&name)?;
		manifest.set_source(&name, &source);
		Ok(changed)
	} else {
		manifest.add_dependency(&requirement.raw)
	}
}

fn source_for_requirement(
	requirement: &InputRequirement,
	project_root: &Path,
) -> Result<Option<(String, PonSource)>> {
	match &requirement.input {
		RequirementInput::Path { path, editable } => {
			let name = normalized_name_of(&requirement.input, project_root)?;
			Ok(Some((name, PonSource {
				path:     Some(path.clone()),
				editable: *editable,
				git:      None,
				rev:      None,
			})))
		},
		RequirementInput::Url { url } if is_git_requirement_url(&url.to_string()) => {
			let name = normalized_name_of(&requirement.input, project_root)?;
			Ok(Some((name, PonSource {
				path:     None,
				editable: false,
				git:      Some(url.to_string()),
				rev:      None,
			})))
		},
		RequirementInput::Pep508(requirement) => {
			if let Some(VersionOrUrl::Url(url)) = &requirement.version_or_url {
				let url = url.to_string();
				if is_git_requirement_url(&url) {
					return Ok(Some((requirement.name.as_ref().to_owned(), PonSource {
						path:     None,
						editable: false,
						git:      Some(url),
						rev:      None,
					})));
				}
			}
			Ok(None)
		},
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
		let parsed = VersionSpecifiers::from_str(&merged)
			.map_err(|_| Error::InvalidSpecifier(merged.clone()))?;
		constraints.insert(name.to_owned(), parsed);
	} else {
		constraints.insert(name.to_owned(), specifier.clone());
	}
	Ok(())
}

fn enforce_hash_mode(
	requirements: &[InputRequirement],
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
		hashes
			.entry(name)
			.or_default()
			.extend(requirement.hashes.iter().cloned());
	}

	for dependency in dependencies {
		let name = names::normalize(&dependency.name);
		if !hashes.contains_key(&name) {
			return Err(Error::Cli(format!(
				"in --require-hashes mode, all transitive dependencies must be listed: missing \
				 `{name}`"
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
		},
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
	specifiers.len() == 1
		&& specifiers[0].operator() == &Operator::Equal
		&& !specifiers.to_string().contains('*')
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
		},
		ResolvedArtifact::Sdist(file) => {
			let path = index.fetch_artifact(file)?;
			verify_hash_for_dist(dist, &path, hashes)?;
			let filename = path.to_string_lossy();
			let sdist_record = ResolvedRecord::sdist(dist.name.clone(), version, &path);
			crate::sdist::build_sdist_with_index(layout, &sdist_record, &filename, index)
		},
		ResolvedArtifact::Dir { path, editable } => {
			Ok(ResolvedRecord::dir(dist.name.clone(), version, path.clone(), *editable))
		},
		ResolvedArtifact::Vcs { dir, .. } => {
			Ok(ResolvedRecord::dir(dist.name.clone(), version, dir.clone(), false))
		},
	}
}

fn install_locked_packages(layout: &EnvLayout, lock: &LockFile) -> Result<usize> {
	for package in &lock.packages {
		if package.kind.tool_kind() == Some("cabi-refused") {
			let record = PackageRecord {
				name:    package.name.clone(),
				version: package.version.clone(),
				kind:    PackageKind::CAbiRefused {
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

fn locked_package_record(
	layout: &EnvLayout,
	package: &crate::lock::LockedPackage,
) -> Result<ResolvedRecord> {
	package.to_resolved_record(&layout.project_root, &cache_root(layout))
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
		},
		ResolvedArtifact::Dir { .. } => {
			return Err(Error::Cli(format!("cannot download directory requirement `{}`", dist.name)));
		},
		ResolvedArtifact::Vcs { .. } => {
			return Err(Error::Cli(format!("cannot download VCS requirement `{}`", dist.name)));
		},
	};
	fs::copy(source, dest.join(filename))?;
	Ok(())
}

fn verify_hash_for_dist(
	dist: &ResolvedDist,
	path: &Path,
	hashes: Option<&HashMap<String, Vec<String>>>,
) -> Result<()> {
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
	if expected
		.iter()
		.any(|hash| hash.eq_ignore_ascii_case(&actual))
	{
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
	path
		.file_name()
		.and_then(|name| name.to_str())
		.unwrap_or("artifact")
}

fn reject_cabi_roots(
	index: &impl PackageIndex,
	requirements: &[InputRequirement],
	allow_prerelease: bool,
) -> Result<()> {
	let resolver = ResolveProvider::new(index).with_allow_prerelease(allow_prerelease);
	for requirement in requirements {
		let record = resolver.resolve_input(&requirement.raw, "")?;
		reject_cabi_package(&record)?;
	}
	Ok(())
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
		name:    dist.name.clone(),
		version: dist.version.to_string(),
		kind:    dist.kind.clone(),
	}
}

fn cabi_error(record: &PackageRecord) -> Error {
	let reason = match &record.kind {
		PackageKind::CAbiRefused { reason } => format!(": {reason}"),
		_ => String::new(),
	};
	Error::UnsupportedArtifact(format!(
		"package `{}` requires the CPython C-ABI (ob_refcnt){reason}; this is a by-design \
		 limitation of pon",
		record.name
	))
}
fn write_lock(
	layout: &EnvLayout,
	dependencies: &[ResolvedDist],
	input_hash: &str,
	index: &impl PackageIndex,
) -> Result<()> {
	fs::create_dir_all(&layout.project_root)?;
	let resolution = Resolution { dists: dependencies.to_vec() };
	let mut lock = LockFile::from_resolution(&resolution, DEFAULT_REQUIRES_PYTHON, input_hash);
	enrich_lock_wheels(&mut lock, dependencies, index)?;
	lock.write_to_path(layout.project_root.join("pon.lock"))
}

fn enrich_lock_wheels(
	lock: &mut LockFile,
	dependencies: &[ResolvedDist],
	index: &impl PackageIndex,
) -> Result<()> {
	for package in &mut lock.packages {
		let Some(dist) = dependencies
			.iter()
			.find(|dist| names::normalize(&dist.name) == names::normalize(&package.name))
		else {
			continue;
		};
		if !matches!(dist.artifact, ResolvedArtifact::Wheel(_)) {
			continue;
		}
		let Some(page) = index.lookup(&dist.name)? else {
			continue;
		};
		let mut wheels = page
			.files
			.into_iter()
			.filter(|file| file.version == dist.version)
			.filter(|file| file.filename.ends_with(".whl"))
			.filter(|file| matches!(file.kind, PackageKind::Pure))
			.map(|file| crate::lock::LockedWheel::from_project_file(&file))
			.collect::<Vec<_>>();
		wheels.sort_by(|left, right| left.name.cmp(&right.name).then_with(|| left.url.cmp(&right.url)));
		wheels.dedup_by(|left, right| left.name == right.name && left.url == right.url);
		if !wheels.is_empty() {
			package.wheels = wheels;
		}
	}
	Ok(())
}

fn manifest_input_hash(
	manifest: &PyProject,
	manifest_path: &Path,
	layout: &EnvLayout,
	index_url: Option<&str>,
	extra_index_urls: &[String],
	no_index: bool,
	find_links: &[PathBuf],
	options: &ResolveOptions,
	_index_shape: &'static str,
) -> Result<String> {
	let effective = effective_index_url(manifest_path, layout, index_url, no_index)?;
	let actual_index_shape = if no_index {
		"local"
	} else if effective == "catalog:" {
		"catalog"
	} else {
		"simple-json"
	};
	let mut entries = vec![
		format!("tool.pon.index-url={effective}"),
		format!("tool.pon.no-index={no_index}"),
		format!("tool.pon.allow-prerelease={}", options.allow_prerelease),
		format!("tool.pon.no-deps={}", options.no_deps),
		format!("tool.pon.index-shape={actual_index_shape}"),
	];
	for url in extra_index_urls {
		entries.push(format!("tool.pon.extra-index-url={url}"));
	}
	for path in find_links {
		entries.push(format!("tool.pon.find-links={}", path.display()));
	}
	for (name, specifiers) in options.constraints.iter() {
		entries.push(format!("constraints.{name}={specifiers}"));
	}
	for (name, source) in manifest.sources() {
		entries.push(format!(
			"tool.pon.sources.{name}=path:{} editable:{} git:{} rev:{}",
			source
				.path
				.as_ref()
				.map_or_else(String::new, |path| path.display().to_string()),
			source.editable,
			source.git.as_deref().unwrap_or_default(),
			source.rev.as_deref().unwrap_or_default()
		));
	}
	Ok(compute_input_hash_with_entries(
		manifest.dependencies().iter().map(String::as_str),
		entries,
	))
}

fn layout_for_manifest(manifest_path: &Path) -> EnvLayout {
	let root = manifest_path
		.parent()
		.filter(|parent| !parent.as_os_str().is_empty())
		.map_or_else(|| PathBuf::from("."), Path::to_path_buf);
	EnvLayout::new(root)
}

/// Nearest ancestor of the cwd with a `pyproject.toml` or `.pon/` marker.
fn discover_project() -> Result<Option<EnvLayout>> {
	let cwd = env::current_dir()?;
	for ancestor in cwd.ancestors() {
		if ancestor.join("pyproject.toml").is_file() || ancestor.join(".pon").is_dir() {
			return Ok(Some(EnvLayout::new(ancestor)));
		}
	}
	Ok(None)
}

fn discover_layout() -> Result<EnvLayout> {
	Ok(match discover_project()? {
		Some(layout) => layout,
		None => EnvLayout::new(env::current_dir()?),
	})
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
	find_links: &[PathBuf],
	allow_unhashed: bool,
) -> Result<SelectedIndex> {
	if no_index {
		return Ok(SelectedIndex::local(cache_root(layout), find_links.to_vec()));
	}
	let base_url = effective_index_url(manifest_path, layout, index_url, false)?;
	if base_url == "catalog:" {
		ensure_catalog_fixture_allowed()?;
		Ok(SelectedIndex::catalog())
	} else {
		let mut index_urls = Vec::with_capacity(extra_index_urls.len() + 1);
		index_urls.push(base_url);
		index_urls.extend(extra_index_urls.iter().cloned());
		Ok(SelectedIndex::simple_json(index_urls, pon_home(layout), allow_unhashed))
	}
}

fn ensure_catalog_fixture_allowed() -> Result<()> {
	if cfg!(test) || env::var_os("PON_TEST_ALLOW_CATALOG").is_some() {
		Ok(())
	} else {
		Err(Error::Cli(
			"`catalog:` is a test fixture index; set PON_TEST_ALLOW_CATALOG=1 for hermetic tests"
				.to_owned(),
		))
	}
}

fn index_shape(index_url: Option<&str>, no_index: bool) -> &'static str {
	if no_index {
		"local"
	} else if index_url == Some("catalog:") {
		"catalog"
	} else {
		"simple-json"
	}
}

fn effective_index_url(
	manifest_path: &Path,
	_layout: &EnvLayout,
	index_url: Option<&str>,
	no_index: bool,
) -> Result<String> {
	if no_index {
		return Ok("no-index".to_owned());
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

fn merged_find_links(cli: &[PathBuf], files: &[PathBuf]) -> Vec<PathBuf> {
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
	requirements: &[InputRequirement],
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
			dependency.artifact = ResolvedArtifact::Dir { path: path.clone(), editable: true };
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
	println!(
		"Summary: {}",
		metadata
			.and_then(|metadata| metadata.summary.as_deref())
			.unwrap_or("")
	);
	println!(
		"Home-page: {}",
		metadata
			.and_then(|metadata| metadata.home_page.as_deref())
			.unwrap_or("")
	);
	println!(
		"Author: {}",
		metadata
			.and_then(|metadata| metadata.author.as_deref())
			.unwrap_or("")
	);
	println!(
		"Author-email: {}",
		metadata
			.and_then(|metadata| metadata.author_email.as_deref())
			.unwrap_or("")
	);
	println!(
		"License: {}",
		metadata
			.and_then(|metadata| metadata.license.as_deref())
			.unwrap_or("")
	);
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

fn read_installed_core_metadata(
	layout: &EnvLayout,
	package: &InstalledPackageRecord,
) -> Result<Option<CoreMetadata>> {
	let Some(text) = read_installed_metadata_text(layout, package)? else {
		return Ok(None);
	};
	parse_core_metadata(&text, &package.name).map(Some)
}

fn read_installed_metadata_text(
	layout: &EnvLayout,
	package: &InstalledPackageRecord,
) -> Result<Option<String>> {
	if let Some(record_path) = &package.record_path {
		if let Some(parent) = record_path.parent() {
			let metadata_path = layout.site_packages.join(parent).join("METADATA");
			if metadata_path.is_file() {
				return fs::read_to_string(metadata_path)
					.map(Some)
					.map_err(Into::into);
			}
		}
	}
	let Some(dist_info) = find_dist_info_dir(layout, package)? else {
		return Ok(None);
	};
	let metadata_path = dist_info.join("METADATA");
	if metadata_path.is_file() {
		fs::read_to_string(metadata_path)
			.map(Some)
			.map_err(Into::into)
	} else {
		Ok(None)
	}
}

fn check_record_integrity(
	layout: &EnvLayout,
	package: &InstalledPackageRecord,
) -> Result<Vec<String>> {
	let Some(record_path) = installed_record_path(layout, package)? else {
		return Ok(vec![format!("package `{}` is missing RECORD", package.name)]);
	};
	if !record_path.is_file() {
		return Ok(vec![format!(
			"package `{}` RECORD `{}` is missing",
			package.name,
			record_path.display()
		)]);
	}
	let label = record_path.display().to_string();
	let record_text = fs::read_to_string(&record_path)?;
	let entries = parse_record(&record_text, &label)?;
	let mut violations = Vec::new();
	for entry in entries {
		let Some(relative) = safe_record_entry_path(&entry.path) else {
			violations.push(format!(
				"package `{}` RECORD contains unsafe path `{}`",
				package.name, entry.path
			));
			continue;
		};
		let path = layout.site_packages.join(&relative);
		if !path.exists() {
			violations.push(format!(
				"package `{}` RECORD entry `{}` is missing",
				package.name, entry.path
			));
			continue;
		}
		if let Some(expected_size) = entry.size {
			let actual_size = fs::metadata(&path)?.len();
			if actual_size != expected_size {
				violations.push(format!(
					"package `{}` RECORD entry `{}` size mismatch: expected {}, got {}",
					package.name, entry.path, expected_size, actual_size
				));
			}
		}
		if let Some(expected_hash) = entry.hash.as_deref() {
			if let Some(expected) = expected_hash.strip_prefix("sha256=") {
				let actual = record_sha256(&path)?;
				if expected != actual {
					violations.push(format!(
						"package `{}` RECORD entry `{}` hash mismatch: expected sha256={}, got \
						 sha256={}",
						package.name, entry.path, expected, actual
					));
				}
			} else {
				violations.push(format!(
					"package `{}` RECORD entry `{}` uses unsupported hash `{}`",
					package.name, entry.path, expected_hash
				));
			}
		}
	}
	Ok(violations)
}

fn installed_record_path(
	layout: &EnvLayout,
	package: &InstalledPackageRecord,
) -> Result<Option<PathBuf>> {
	if let Some(record_path) = &package.record_path {
		return Ok(Some(layout.site_packages.join(record_path)));
	}
	Ok(find_dist_info_dir(layout, package)?.map(|dist_info| dist_info.join("RECORD")))
}

fn safe_record_entry_path(path: &str) -> Option<PathBuf> {
	let candidate = Path::new(path);
	let is_unsafe = candidate.components().any(|component| {
		matches!(
			component,
			Component::Prefix(_) | Component::RootDir | Component::ParentDir
		)
	});
	if is_unsafe {
		None
	} else {
		Some(candidate.to_path_buf())
	}
}

fn record_sha256(path: &Path) -> Result<String> {
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
	Ok(URL_SAFE_NO_PAD.encode(hasher.finalize()))
}

fn find_dist_info_dir(
	layout: &EnvLayout,
	package: &InstalledPackageRecord,
) -> Result<Option<PathBuf>> {
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

fn required_by(
	package: &InstalledPackageRecord,
	metadata: &HashMap<String, CoreMetadata>,
) -> Vec<String> {
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

fn installed_satisfies_requirement(
	package: &InstalledPackageRecord,
	requirement: &Requirement,
) -> Result<bool> {
	let version = Version::from_str(&package.version)
		.map_err(|_| Error::InvalidRequirement(package.version.clone()))?;
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
			"pon-{name}-{}-{}",
			std::process::id(),
			std::time::SystemTime::now()
				.duration_since(std::time::UNIX_EPOCH)
				.expect("clock")
				.as_nanos()
		);
		std::env::temp_dir().join(unique)
	}

	struct EnvGuard {
		key: &'static str,
		previous: Option<std::ffi::OsString>,
	}

	impl EnvGuard {
		fn set(key: &'static str, value: &str) -> Self {
			let previous = std::env::var_os(key);
			unsafe {
				std::env::set_var(key, value);
			}
			Self { key, previous }
		}
	}

	impl Drop for EnvGuard {
		fn drop(&mut self) {
			unsafe {
				if let Some(previous) = &self.previous {
					std::env::set_var(self.key, previous);
				} else {
					std::env::remove_var(self.key);
				}
			}
		}
	}

	#[test]
	fn clap_accepts_manifest_and_index_urls_for_resolving_commands() {
		let cli = Cli::try_parse_from([
			"pon",
			"install",
			"--manifest",
			"demo.toml",
			"--index-url",
			"https://packages.example/simple/",
			"--extra-index-url",
			"https://mirror-one.example/simple/",
			"--extra-index-url",
			"https://mirror-two.example/simple/",
			"--find-links",
			"wheelhouse",
		])
		.expect("parse");

		let Command::Install(options) = cli.command else {
			panic!("install command");
		};
		assert_eq!(options.manifest.manifest, PathBuf::from("demo.toml"));
		assert_eq!(options.index.index_url.as_deref(), Some("https://packages.example/simple/"));
		assert_eq!(options.index.extra_index_url, vec![
			"https://mirror-one.example/simple/".to_owned(),
			"https://mirror-two.example/simple/".to_owned()
		]);
		assert_eq!(options.index.find_links, vec![PathBuf::from("wheelhouse")]);
	}

	#[test]
	fn clap_accepts_install_requirement_file_and_flags() {
		let cli = Cli::try_parse_from([
			"pon",
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
			"--allow-unhashed",
			"--find-links",
			"wheelhouse",
		])
		.expect("parse");

		let Command::Install(options) = cli.command else {
			panic!("install command");
		};
		assert_eq!(options.requirements, vec!["requests==2.32.0".to_owned()]);
		assert_eq!(options.requirement_files, vec![PathBuf::from("requirements.txt")]);
		assert_eq!(options.constraint_files, vec![PathBuf::from("constraints.txt")]);
		assert_eq!(options.editables, vec![PathBuf::from("./pkg")]);
		assert!(options.no_deps);
		assert!(options.pre);
		assert!(options.require_hashes);
		assert!(options.dry_run);
		assert!(options.frozen);
		assert!(options.no_index);
		assert!(options.allow_unhashed);
		assert_eq!(options.index.find_links, vec![PathBuf::from("wheelhouse")]);
	}

	#[test]
	fn hash_mode_rejects_unpinned_missing_hash_unverifiable_and_unlisted_transitives() {
		let root = temp_project("hash-mode");
		fs::create_dir_all(&root).expect("root");
		let cases = [
			(
				input_requirement("demo>=1", ["sha256:abc"]),
				Vec::new(),
				"in --require-hashes mode, all requirements must be pinned with ==: `demo>=1`",
			),
			(
				input_requirement("demo==1", []),
				Vec::new(),
				"in --require-hashes mode, all requirements must have a --hash: `demo==1`",
			),
			(
				editable_requirement("./pkg"),
				Vec::new(),
				"cannot verify hashes for editable requirement `-e ./pkg`",
			),
			(
				input_requirement("git+https://example.test/demo.git@v1", ["sha256:abc"]),
				Vec::new(),
				"cannot verify hashes for VCS requirement `git+https://example.test/demo.git@v1`",
			),
			(
				input_requirement("demo==1", ["sha256:abc"]),
				vec![resolved_dist("demo"), resolved_dist("dep")],
				"in --require-hashes mode, all transitive dependencies must be listed: missing `dep`",
			),
		];

		for (requirement, dependencies, expected) in cases {
			let error = enforce_hash_mode(&[requirement], true, &root, &dependencies)
				.expect_err("hash mode should reject invalid input");
			assert_eq!(error.to_string(), expected);
		}
	}

	#[test]
	fn hash_mode_maps_pinned_registry_requirements_to_hashes() {
		let root = temp_project("hash-ok");
		fs::create_dir_all(&root).expect("root");
		let requirements = [
			input_requirement("demo==1", ["sha256:aaa"]),
			input_requirement("demo==1", ["sha256:bbb"]),
		];

		let hashes = enforce_hash_mode(&requirements, true, &root, &[resolved_dist("demo")])
			.expect("hash mode")
			.expect("hashes");

		assert_eq!(hashes.get("demo").expect("demo hashes"), &vec![
			"sha256:aaa".to_owned(),
			"sha256:bbb".to_owned()
		]);
	}

	#[test]
	fn clap_accepts_run_file_args_and_entry_args_without_runtime_execution() {
		let file_cli = Cli::try_parse_from(["pon", "run", "app.py", "--", "-x", "value"])
			.expect("parse file run");
		let Command::Run(file_run) = file_cli.command else {
			panic!("run command");
		};
		let mut file_args = file_run.args.clone();
		strip_arg_delimiter(&mut file_args, 1);
		assert_eq!(file_run.code, None);
		assert_eq!(file_run.entry, None);
		assert_eq!(file_args, vec!["app.py".to_owned(), "-x".to_owned(), "value".to_owned()]);

		let entry_cli =
			Cli::try_parse_from(["pon", "run", "--entry", "demo_pkg.cli:main", "--", "a", "b"])
				.expect("parse entry run");
		let Command::Run(entry_run) = entry_cli.command else {
			panic!("run command");
		};
		let mut entry_args = entry_run.args.clone();
		strip_arg_delimiter(&mut entry_args, 0);
		assert_eq!(entry_run.entry.as_deref(), Some("demo_pkg.cli:main"));
		assert_eq!(entry_args, vec!["a".to_owned(), "b".to_owned()]);
	}

	#[test]
	fn init_creates_manifest_and_dot_pon_dirs() {
		let root = temp_project("init");
		let manifest = root.join("pyproject.toml");

		run_from_args([
			"pon".to_owned(),
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
		let _catalog = EnvGuard::set("PON_TEST_ALLOW_CATALOG", "1");

		run_from_args([
			"pon".to_owned(),
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
		assert!(
			root
				.join(".pon/packages/site-packages/idna/__init__.py")
				.is_file()
		);
		let lock = fs::read_to_string(root.join("pon.lock")).expect("lock");
		assert!(lock.contains("name = \"idna\""));
		assert!(lock.contains("version = \"3.10\""));
	}

	#[test]
	fn add_local_sdist_records_source_and_installs_package() {
		let root = temp_project("add-sdist");
		let manifest = root.join("pyproject.toml");
		fs::create_dir_all(&root).expect("root");
		let _catalog = EnvGuard::set("PON_TEST_ALLOW_CATALOG", "1");
		let sdist_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
			.join("fixtures")
			.join("sdists")
			.join("pon-flit-fixture-0.1.0.tar.gz");
		let raw_path = sdist_path.display().to_string();
		let _guard = EnvGuard::set("PON_TEST_ALLOW_FIXTURE_BRIDGE", "1");

		run_from_args([
			"pon".to_owned(),
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
		let _catalog = EnvGuard::set("PON_TEST_ALLOW_CATALOG", "1");
		run_from_args([
			"pon".to_owned(),
			"add".to_owned(),
			"idna".to_owned(),
			"--manifest".to_owned(),
			manifest.display().to_string(),
			"--index-url".to_owned(),
			"catalog:".to_owned(),
		])
		.expect("add");

		run_from_args([
			"pon".to_owned(),
			"remove".to_owned(),
			"idna".to_owned(),
			"--manifest".to_owned(),
			manifest.display().to_string(),
		])
		.expect("remove");


		let pyproject = fs::read_to_string(&manifest).expect("pyproject");
		assert!(!pyproject.contains("\"idna\""));
		assert!(!root.join(".pon/packages/site-packages/idna").exists());
		assert!(
			!root
				.join(".pon/packages/site-packages/idna-3.10.dist-info")
				.exists()
		);
		let registry =
			fs::read_to_string(root.join(".pon/packages/installed.tsv")).expect("registry");
		assert!(registry.is_empty());
	}

	#[test]
	fn locked_install_rejects_tampered_cached_wheel_hash() {
		let root = temp_project("tampered-lock");
		let layout = EnvLayout::new(&root);
		let cache_wheel_dir = cache_root(&layout).join("wheels").join("tampered");
		fs::create_dir_all(&cache_wheel_dir).expect("cache dir");
		let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
			.join("fixtures")
			.join("wheels")
			.join("idna-3.10-py3-none-any.whl");
		let expected_hash = sha256_file(&fixture).expect("fixture hash");
		let cached_wheel = cache_wheel_dir.join("idna-3.10-py3-none-any.whl");
		fs::write(&cached_wheel, b"tampered wheel bytes").expect("tamper cached wheel");
		let mut hashes = BTreeMap::new();
		hashes.insert("sha256".to_owned(), expected_hash);
		let mut package = crate::lock::LockedPackage::pure("idna", "3.10");
		package.wheels.push(crate::lock::LockedWheel {
			name:   "idna-3.10-py3-none-any.whl".to_owned(),
			url:    format!("file://{}", cached_wheel.display()),
			hashes,
		});
		let lock = LockFile::with_input_hash(
			vec![package],
			DEFAULT_REQUIRES_PYTHON,
			"sha256:tampered-test",
		);

		let error = install_locked_packages(&layout, &lock).expect_err("tampered wheel rejected");

		assert!(
			error.to_string().contains("hash mismatch"),
			"expected hash mismatch, got {error}"
		);
	}


	#[test]
	fn check_record_integrity_reports_record_violations() {
		let root = temp_project("record-check");
		let layout = EnvLayout::new(&root);
		let package_dir = layout.site_packages.join("demo");
		let dist_info = layout.site_packages.join("demo-1.0.dist-info");
		fs::create_dir_all(&package_dir).expect("package dir");
		fs::create_dir_all(&dist_info).expect("dist-info");
		let module_path = package_dir.join("__init__.py");
		fs::write(&module_path, b"ok\n").expect("module");
		let module_hash = record_sha256(&module_path).expect("hash");
		let module_size = fs::metadata(&module_path).expect("metadata").len();
		fs::write(
			dist_info.join("RECORD"),
			format!(
				"demo/__init__.py,sha256={module_hash},{module_size}\n../escape.py,,\ndemo-1.0.dist-info/RECORD,,\n"
			),
		)
		.expect("record");
		let package = InstalledPackageRecord {
			name:          "demo".to_owned(),
			version:       "1.0".to_owned(),
			artifact_kind: "wheel".to_owned(),
			import_names:  vec!["demo".to_owned()],
			record_path:   Some(PathBuf::from("demo-1.0.dist-info/RECORD")),
		};

		let violations = check_record_integrity(&layout, &package).expect("record check");
		assert_eq!(violations.len(), 1);
		assert!(violations[0].contains("unsafe path"));

		fs::write(&module_path, b"tampered\n").expect("tamper");
		let violations = check_record_integrity(&layout, &package).expect("record check");

		assert!(violations.iter().any(|violation| violation.contains("hash mismatch")));
		assert!(violations.iter().any(|violation| violation.contains("size mismatch")));
		let _ = fs::remove_dir_all(root);
	}
	#[test]
	fn add_refuses_c_abi_catalog_package() {
		let root = temp_project("numpy");
		let manifest = root.join("pyproject.toml");
		fs::create_dir_all(&root).expect("root");
		let _catalog = EnvGuard::set("PON_TEST_ALLOW_CATALOG", "1");

		let error = run_from_args([
			"pon".to_owned(),
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
			.map(|(key, value)| {
				(key.to_string_lossy().into_owned(), value.to_string_lossy().into_owned())
			})
			.collect::<std::collections::BTreeMap<_, _>>();

		assert_eq!(env["PON_HOME"], "/tmp/project/.pon");
		assert!(env["PONPATH"].contains("/tmp/project/.pon/packages/site-packages"));
		assert_eq!(env["PONPATH"], env["PON_IMPORT_PATH"]);
		assert_eq!(env["PON_NATIVE_MODULE_REGISTRY"], "/tmp/project/.pon/native/registry.tsv");
	}

	fn input_requirement<const N: usize>(raw: &str, hashes: [&str; N]) -> InputRequirement {
		InputRequirement {
			raw:    raw.to_owned(),
			input:  parse_requirement_input(raw).expect("requirement"),
			hashes: hashes.into_iter().map(str::to_owned).collect(),
		}
	}

	fn editable_requirement(raw: &str) -> InputRequirement {
		InputRequirement {
			raw:    format!("-e {raw}"),
			input:  RequirementInput::Path { path: PathBuf::from(raw), editable: true },
			hashes: vec!["sha256:abc".to_owned()],
		}
	}

	fn resolved_dist(name: &str) -> ResolvedDist {
		ResolvedDist {
			name:         name.to_owned(),
			version:      Version::from_str("1").expect("version"),
			kind:         PackageKind::Pure,
			artifact:     ResolvedArtifact::Dir {
				path:     PathBuf::from("/unused"),
				editable: false,
			},
			dependencies: Vec::new(),
			marker:       None,
		}
	}
}
