#![doc = "Library-first entry points for running and building Pon programs."]

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use pon_ir::lower_source;
use pon_jit::JitEngine;
use pon_runtime::dynexec::{DynCodeMode, DynCompileRequest, DynExecuteRequest, set_dynamic_code_hooks};
use pon_runtime::import::{SourceModuleRequest, active_module_attr, begin_module_execution, cached_module, end_module_execution, install_module, set_source_module_loader};
use pon_runtime::{PyObject, intern, pon_none, pon_runtime_init};

pub mod build;

/// Dispatches the process command line using the same behavior as the `pon-cli` binary.
pub fn run_from_env() -> Result<()> {
    run_from_args(env::args())
}

/// Dispatches a `pon` command-line argument vector.
///
/// The iterator must include argv[0]. This shape keeps binary dispatch and tests
/// using one parser while allowing `pon-pm` to delegate to the same implementation.
pub fn run_from_args(args: impl IntoIterator<Item = String>) -> Result<()> {
    let mut args = args.into_iter();
    let program = args.next().unwrap_or_else(|| "pon".to_owned());
    match args.next().as_deref() {
        Some("run") => {
            let file = args.next().context("missing file for `pon run <file>`")?;
            if let Some(extra) = args.next() {
                bail!("unexpected argument `{extra}` for `pon run <file>`");
            }
            run_file(file)
        }
        Some("build") => build_from_args(args),
        Some("repl") => bail!("`pon repl` is unsupported in Phase A"),
        Some(command) => bail!("unknown subcommand `{command}`\n{}", usage(&program)),
        None => bail!(usage(program)),
    }
}

/// Runs one Pon/Python source file through the JIT backend using the current process environment.
pub fn run_file(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    let source = fs::read_to_string(path)
        .with_context(|| format!("failed to read UTF-8 source `{}`", path.display()))?;
    let mut script_path_guard = EnvOverlay::new();
    if let Some(script_dir) = path.parent() {
        let mut roots = vec![script_dir.as_os_str().to_os_string()];
        if let Some(existing) = env::var_os("PONPATH") {
            roots.extend(env::split_paths(&existing).map(|path| path.into_os_string()));
        }
        let joined = env::join_paths(roots).context("failed to build import search path")?;
        script_path_guard.set(OsString::from("PONPATH"), joined);
    }
    let module = lower_source(&source).context("failed to parse/lower source")?;
    set_source_module_loader(load_source_module);
    set_dynamic_code_hooks(validate_dynamic_source, execute_dynamic_source);
    let init_status = unsafe { pon_runtime_init() };
    if init_status != 0 {
        bail!("runtime initialization failed");
    }
    install_module("__main__", []).map_err(anyhow::Error::msg)?;
    let mut engine = JitEngine::new();
    begin_module_execution("__main__").map_err(anyhow::Error::msg)?;
    let result = engine.run(&module).context("JIT execution failed");
    end_module_execution("__main__");
    result?;
    io::stdout().flush().context("failed to flush stdout")
}

fn load_source_module(request: SourceModuleRequest<'_>) -> std::result::Result<*mut pon_runtime::PyObject, String> {
    let module = lower_source(request.source).map_err(|error| format!("failed to parse/lower source module '{}': {error}", request.path.display()))?;
    let mut engine = JitEngine::new();
    engine
        .run(&module)
        .map_err(|error| format!("failed to execute source module '{}': {error}", request.name))?;
    std::mem::forget(engine);
    cached_module(intern(request.name)).ok_or_else(|| format!("source module '{}' was not cached", request.name))
}

fn dynexec_source(source: &str, mode: DynCodeMode) -> String {
    match mode {
        DynCodeMode::Eval => format!("__pon_dyn_eval_result = ({source})\n"),
        DynCodeMode::Exec | DynCodeMode::Single => source.to_owned(),
    }
}

fn validate_dynamic_source(request: DynCompileRequest<'_>) -> std::result::Result<(), String> {
    let source = dynexec_source(request.source, request.mode);
    lower_source(&source)
        .map(|_| ())
        .map_err(|error| format!("failed to parse/lower dynamic source '{}': {error}", request.filename))
}

fn execute_dynamic_source(request: DynExecuteRequest<'_>) -> std::result::Result<*mut PyObject, String> {
    let source = dynexec_source(request.source, request.mode);
    let module = lower_source(&source)
        .map_err(|error| format!("failed to parse/lower dynamic source '{}': {error}", request.filename))?;
    let mut engine = JitEngine::new();
    engine
        .run(&module)
        .map_err(|error| format!("failed to execute dynamic source '{}': {error}", request.filename))?;
    std::mem::forget(engine);
    match request.mode {
        DynCodeMode::Eval => {
            let name = intern("__pon_dyn_eval_result");
            active_module_attr(name).ok_or_else(|| "dynamic eval did not produce a result".to_owned())
        }
        DynCodeMode::Exec | DynCodeMode::Single => {
            let none = unsafe { pon_none() };
            if none.is_null() {
                Err("failed to allocate None for dynamic exec result".to_owned())
            } else {
                Ok(none)
            }
        }
    }
}

/// Runs one Pon/Python source file with additional environment visible to the runtime.
///
/// This is the library-first hook used by package-manager delegation when it
/// needs to expose managed import roots or a native-module registry without
/// changing the `pon run <file>` command-line behavior.
pub fn run_file_with_env<I, K, V>(path: impl AsRef<Path>, extra_env: I) -> Result<()>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<OsString>,
    V: Into<OsString>,
{
    let mut guard = EnvOverlay::new();
    for (key, value) in extra_env {
        guard.set(key.into(), value.into());
    }
    run_file(path)
}

struct EnvOverlay {
    previous: Vec<(OsString, Option<OsString>)>,
}

impl EnvOverlay {
    fn new() -> Self {
        Self { previous: Vec::new() }
    }

    fn set(&mut self, key: OsString, value: OsString) {
        let previous = env::var_os(&key);
        unsafe {
            env::set_var(&key, value);
        }
        self.previous.push((key, previous));
    }
}

impl Drop for EnvOverlay {
    fn drop(&mut self) {
        for (key, previous) in self.previous.drain(..).rev() {
            unsafe {
                if let Some(value) = previous {
                    env::set_var(key, value);
                } else {
                    env::remove_var(key);
                }
            }
        }
    }
}

/// Builds one Pon/Python source file through the AoT backend.
pub fn build_from_args(args: impl IntoIterator<Item = String>) -> Result<()> {
    build::run_from_args(args)
}

fn usage(program: impl AsRef<str>) -> String {
    let program = program.as_ref();
    format!(
        "usage: {program} run <file>\n       {program} build <file> -o <out> [--allow-dynamic] [--opt] [--target <triple>]"
    )
}
