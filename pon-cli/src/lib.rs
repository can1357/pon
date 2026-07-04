#![doc = "Library-first entry points for running and building Pon programs."]

use std::env;
use std::ffi::{CString, OsString};
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use pon_ir::lower_source;
use pon_jit::JitEngine;
use pon_runtime::dynexec::{DynCodeMode, DynCompileRequest, DynExecuteRequest, set_ast_parse_hook, set_dynamic_code_hooks};
use pon_runtime::import::{SourceModuleRequest, active_module_attr, begin_module_execution, cached_module, end_module_execution, install_module, set_source_module_loader};
use pon_runtime::{PyObject, intern, pon_const_str, pon_none, pon_runtime_init, pon_sys_set_argv};

mod astconv;
pub mod build;


/// A `sys.exit(code)` request surfaced from a top-level run.  The CLI entry
/// point (`main`) maps it to the process exit status; embedded callers such as
/// `pon-pm`'s in-process build hooks treat it as an ordinary error so they can
/// report a backend failure instead of terminating their own process.
#[derive(Debug)]
pub struct SystemExitRequested(pub i32);

impl std::fmt::Display for SystemExitRequested {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SystemExit: {}", self.0)
    }
}

impl std::error::Error for SystemExitRequested {}
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
        Some(command) => {
            // Python-compatible script invocation: `pon <script> [args...]`
            // runs the file directly (like `python script.py args`), so tools
            // that spawn `[sys.executable, <script>, ...]` (e.g. meson-python
            // invoking meson) work.  Gated on `command` naming an existing file
            // so real subcommand typos still error out.
            let script = Path::new(command);
            if script.is_file() {
                let mut argv = vec![command.to_owned()];
                argv.extend(args);
                return run_file_with_env(script, std::iter::empty::<(OsString, OsString)>(), &argv);
            }
            bail!("unknown subcommand `{command}`\n{}", usage(&program));
        }
        None => bail!(usage(program)),
    }
}

/// Runs one Pon/Python source file through the JIT backend using the current process environment.
///
/// The runtime receives a `sys.argv` containing the source path as argv[0].
pub fn run_file(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    let argv = vec![path.to_string_lossy().into_owned()];
    run_file_with_env(path, std::iter::empty::<(OsString, OsString)>(), &argv)
}

fn script_module_attrs(path: &Path) -> Result<Vec<(u32, *mut PyObject)>> {
    let file_path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let file_text = file_path.to_string_lossy();
    let file_object = unsafe { pon_const_str(file_text.as_ptr(), file_text.len()) };
    if file_object.is_null() {
        let detail = pon_runtime::pon_err_message().unwrap_or_else(|| format!("failed to allocate __file__ for `{}`", path.display()));
        bail!(detail);
    }
    let cached = unsafe { pon_none() };
    if cached.is_null() {
        bail!("failed to allocate None for __cached__");
    }
    Ok(vec![(intern("__file__"), file_object), (intern("__cached__"), cached)])
}

fn run_file_inner(path: &Path, argv: &[String]) -> Result<()> {
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
    set_ast_parse_hook(astconv::parse_dynamic_ast);
    let init_status = unsafe { pon_runtime_init() };
    if init_status != 0 {
        bail!("runtime initialization failed");
    }
    // Conservative-stack root boundary for the JIT, mirroring `pon_aot_entry`:
    // all generated code runs in frames below this one, so a `gc.collect()`
    // triggered inside JIT code scans the native stack range holding JIT frame
    // locals.  Without the boundary the collector sees no stack roots at all
    // and frees objects held only by JIT frame slots (locals across collect).
    let mut stack_base_marker = 0usize;
    pon_runtime::aot_entry::capture_stack_base(std::ptr::addr_of_mut!(stack_base_marker).cast::<u8>());
    let mut argv_cstrings = Vec::with_capacity(argv.len());
    for arg in argv {
        let c_arg = match CString::new(arg.as_str()) {
            Ok(c_arg) => c_arg,
            Err(_) => bail!("argv contains NUL byte"),
        };
        argv_cstrings.push(c_arg);
    }
    let argv_ptrs = argv_cstrings
        .iter()
        .map(|arg| arg.as_ptr().cast::<u8>())
        .collect::<Vec<_>>();
    if unsafe { pon_sys_set_argv(argv_ptrs.len() as i32, argv_ptrs.as_ptr()) } != 0 {
        bail!("runtime initialization failed");
    }
    install_module("__main__", script_module_attrs(path)?).map_err(anyhow::Error::msg)?;
    let mut engine = JitEngine::new();
    begin_module_execution("__main__").map_err(anyhow::Error::msg)?;
    let result = engine.run(&module).context("JIT execution failed");
    // `sys.exit` raises SystemExit; capture the requested process exit status
    // before finalizers run (CPython runs atexit callbacks, then exits with
    // the code).
    let system_exit = pon_runtime::abi::take_pending_system_exit();
    // atexit callbacks run once `__main__` finishes (normally or raising) but
    // before module teardown, so hooks still see live module state.
    pon_runtime::native::atexit::run_exit_callbacks();
    end_module_execution("__main__");
    if let Some(code) = system_exit {
        io::stdout().flush().context("failed to flush stdout")?;
        return Err(SystemExitRequested(code).into());
    }
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
        DynCodeMode::Exec => source.to_owned(),
        DynCodeMode::Single => dynexec_single_source(source),
    }
}

fn dynexec_single_source(source: &str) -> String {
    let display_source = format!(
        concat!(
            "__pon_dyn_single_result = ({})\n",
            "if __pon_dyn_single_result is not None:\n",
            "    print(repr(__pon_dyn_single_result))\n",
        ),
        source
    );
    if lower_source(&display_source).is_ok() {
        display_source
    } else {
        source.to_owned()
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
/// changing the `pon run <file>` command-line behavior. The provided `argv`
/// slice is installed as runtime `sys.argv` after runtime initialization.
pub fn run_file_with_env<I, K, V>(
    path: impl AsRef<Path>,
    extra_env: I,
    argv: &[String],
) -> Result<()>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<OsString>,
    V: Into<OsString>,
{
    let mut guard = EnvOverlay::new();
    for (key, value) in extra_env {
        guard.set(key.into(), value.into());
    }
    run_file_inner(path.as_ref(), argv)
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
