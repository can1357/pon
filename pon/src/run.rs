#![doc = "Library-first entry points for running Pon programs."]

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

/// A `sys.exit(code)` request surfaced from a top-level run.  The CLI entry
/// point (`main`) maps it to the process exit status; embedded callers such as
/// the package manager's in-process build hooks (`crate::sdist`) treat it as an
/// ordinary error so they can report a backend failure instead of terminating
/// their own process.
#[derive(Debug)]
pub struct SystemExitRequested(pub i32);

impl std::fmt::Display for SystemExitRequested {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SystemExit: {}", self.0)
    }
}

impl std::error::Error for SystemExitRequested {}

/// A top-level runtime exception already rendered through Pon's traceback path.
/// The CLI maps it to failure without adding an `anyhow` prefix, preserving the
/// Python-shaped final exception line for stderr consumers.
#[derive(Debug)]
pub struct UncaughtExceptionReported;

impl std::fmt::Display for UncaughtExceptionReported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("uncaught exception")
    }
}

impl std::error::Error for UncaughtExceptionReported {}
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
        let detail = pon_runtime::pon_err_message()
            .unwrap_or_else(|| format!("failed to allocate __file__ for `{}`", path.display()));
        bail!(detail);
    }
    let cached = unsafe { pon_none() };
    if cached.is_null() {
        bail!("failed to allocate None for __cached__");
    }
    Ok(vec![
        (intern("__file__"), file_object),
        (intern("__cached__"), cached),
    ])
}

fn run_file_inner(path: &Path, argv: &[String]) -> Result<()> {
    run_file_inner_with_package(path, argv, None)
}

/// `pon -m <module>` (CPython `python -m` / runpy): the current directory
/// leads the import path, a dotted module runs with `__name__ = "__main__"`
/// and `__package__` set to its parent package so relative imports resolve,
/// and a package target runs its `__main__` submodule.
pub(crate) fn run_module_as_main(module: &str, extra_args: Vec<String>) -> Result<()> {
    let mut overlay = EnvOverlay::new();
    let cwd = env::current_dir().context("cannot resolve the current directory for `pon -m`")?;
    let mut roots = vec![cwd.clone().into_os_string()];
    if let Some(existing) = env::var_os("PONPATH") {
        roots.extend(env::split_paths(&existing).map(|path| path.into_os_string()));
    }
    overlay.set(
        OsString::from("PONPATH"),
        env::join_paths(roots).context("failed to build import search path")?,
    );

    // Resolve against the same roots the runtime consults: PONPATH (now led
    // by the cwd), PON_IMPORT_PATH, and PYTHONPATH.
    let mut search_roots = vec![cwd];
    for var in ["PONPATH", "PON_IMPORT_PATH", "PYTHONPATH"] {
        if let Some(extra) = env::var_os(var) {
            search_roots.extend(env::split_paths(&extra));
        }
    }
    let resolve = |name: &str| -> Option<(std::path::PathBuf, bool)> {
        let mut relative = std::path::PathBuf::new();
        for part in name.split('.') {
            relative.push(part);
        }
        for root in &search_roots {
            let package_init = root.join(&relative).join("__init__.py");
            if package_init.is_file() {
                return Some((package_init, true));
            }
            let mut module_path = root.join(&relative);
            module_path.set_extension("py");
            if module_path.is_file() {
                return Some((module_path, false));
            }
        }
        None
    };
    let Some((path, is_package)) = resolve(module) else {
        bail!("No module named {module}");
    };
    let (path, package) = if is_package {
        // `python -m pkg` runs `pkg.__main__`.
        let Some((main_path, false)) = resolve(&format!("{module}.__main__")) else {
            bail!("No module named {module}.__main__; '{module}' is a package and cannot be directly executed");
        };
        (main_path, module.to_owned())
    } else {
        let package = module.rsplit_once('.').map(|(parent, _)| parent.to_owned()).unwrap_or_default();
        (path, package)
    };
    let mut argv = vec![path.to_string_lossy().into_owned()];
    argv.extend(extra_args);
    run_file_inner_with_package(&path, &argv, Some(&package))
}

/// How `__main__` is materialized for a top-level run.
enum MainModule<'a> {
    /// `pon run file.py` / `pon file.py` / `pon -m`: file-backed `__main__`.
    Script(&'a Path),
    /// `pon -c` / `pon -`: inline `__main__` without `__file__`.
    Inline,
}

fn inline_module_attrs() -> Result<Vec<(u32, *mut PyObject)>> {
    let cached = unsafe { pon_none() };
    if cached.is_null() {
        bail!("failed to allocate None for __cached__");
    }
    Ok(vec![(intern("__cached__"), cached)])
}

fn run_file_inner_with_package(path: &Path, argv: &[String], main_package: Option<&str>) -> Result<()> {
    let source = fs::read_to_string(path)
        .with_context(|| format!("failed to read UTF-8 source `{}`", path.display()))?;
    let mut script_path_guard = EnvOverlay::new();
    // `pon -m` already placed the cwd at the head of the import path; only
    // direct script runs prepend the script's own directory (CPython
    // `sys.path[0]` semantics for each invocation form).
    if main_package.is_none()
        && let Some(script_dir) = path.parent()
    {
        let mut roots = vec![script_dir.as_os_str().to_os_string()];
        if let Some(existing) = env::var_os("PONPATH") {
            roots.extend(env::split_paths(&existing).map(|path| path.into_os_string()));
        }
        let joined = env::join_paths(roots).context("failed to build import search path")?;
        script_path_guard.set(OsString::from("PONPATH"), joined);
    }
    let module = lower_source(&source).context("failed to parse/lower source")?;
    execute_main(&module, argv, MainModule::Script(path), main_package)
}

/// Installs the JIT dynamic-code hooks, initializes the runtime, records the
/// conservative-stack root boundary, and installs `sys.argv`.
///
/// `stack_base_marker` must point at a local in a caller frame that outlives
/// every JIT frame; the GC scans the native stack down from it.
pub(crate) fn boot_runtime(argv: &[String], stack_base_marker: *mut u8) -> Result<()> {
    set_source_module_loader(load_source_module);
    set_dynamic_code_hooks(validate_dynamic_source, execute_dynamic_source);
    set_ast_parse_hook(crate::astconv::parse_dynamic_ast);
    let init_status = unsafe { pon_runtime_init() };
    if init_status != 0 {
        bail!("runtime initialization failed");
    }
    pon_runtime::aot_entry::capture_stack_base(stack_base_marker);
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
    Ok(())
}

/// Executes an already-lowered `__main__` module.
fn execute_main(
    module: &pon_ir::Module,
    argv: &[String],
    main: MainModule<'_>,
    main_package: Option<&str>,
) -> Result<()> {
    let mut stack_base_marker = 0usize;
    boot_runtime(argv, std::ptr::addr_of_mut!(stack_base_marker).cast::<u8>())?;
    match main {
        MainModule::Script(path) => install_module("__main__", script_module_attrs(path)?),
        MainModule::Inline => install_module("__main__", inline_module_attrs()?),
    }
    .map_err(anyhow::Error::msg)?;
    if let Some(package) = main_package.filter(|package| !package.is_empty()) {
        // runpy: `__main__.__package__` names the parent package so the
        // module's relative imports resolve.
        let package_object = unsafe { pon_const_str(package.as_ptr(), package.len()) };
        if package_object.is_null() {
            bail!("failed to allocate __package__ for `pon -m`");
        }
        pon_runtime::import::store_module_attr(intern("__main__"), intern("__package__"), package_object);
    }
    let mut engine = JitEngine::new();
    begin_module_execution("__main__").map_err(anyhow::Error::msg)?;
    let result = engine.run(module).context("JIT execution failed");
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
    if let Err(error) = result {
        if pon_runtime::pon_err_occurred() {
            unsafe {
                pon_runtime::pon_err_report_uncaught();
            }
            io::stdout().flush().context("failed to flush stdout")?;
            return Err(UncaughtExceptionReported.into());
        }
        return Err(error);
    }
    io::stdout().flush().context("failed to flush stdout")
}

/// Runs an in-memory program as `__main__`.
pub(crate) fn run_inline_source<I, K, V>(source: &str, extra_env: I, argv: &[String]) -> Result<()>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<OsString>,
    V: Into<OsString>,
{
    let mut guard = EnvOverlay::new();
    for (key, value) in extra_env {
        guard.set(key.into(), value.into());
    }
    let cwd = env::current_dir().context("cannot resolve the current directory for inline execution")?;
    let mut roots = vec![cwd.into_os_string()];
    if let Some(existing) = env::var_os("PONPATH") {
        roots.extend(env::split_paths(&existing).map(|path| path.into_os_string()));
    }
    let mut cwd_path_guard = EnvOverlay::new();
    cwd_path_guard.set(
        OsString::from("PONPATH"),
        env::join_paths(roots).context("failed to build import search path")?,
    );
    let module = lower_source(source).context("failed to parse/lower source")?;
    execute_main(&module, argv, MainModule::Inline, None)
}

fn load_source_module(request: SourceModuleRequest<'_>) -> std::result::Result<*mut pon_runtime::PyObject, String> {
    let module = lower_source(request.source).map_err(|error| format!("failed to parse/lower source module '{}': {error}", request.path.display()))?;
    let mut engine = JitEngine::new();
    engine
        .run(&module)
        .map_err(|error| {
            let where_ = pon_runtime::pending_traceback_lines()
                .map(|lines| format!(" [{lines}]"))
                .unwrap_or_default();
            format!("failed to execute source module '{}': {error}{where_}", request.name)
        })?;
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

/// Compiles and runs one interactive entry inside the caller's active `__main__` execution.
pub(crate) fn exec_interactive(source: &str) -> std::result::Result<(), String> {
    let wrapped = dynexec_single_source(source);
    let module = lower_source(&wrapped).map_err(|error| error.to_string())?;
    let mut engine = JitEngine::new();
    engine.run(&module).map_err(|error| error.to_string())?;
    std::mem::forget(engine);
    Ok(())
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

