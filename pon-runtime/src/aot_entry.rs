//! AoT process entrypoint and runtime process-boundary helpers.

use std::io::{self, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;

use crate::object::PyObject;
use crate::thread_state::{pon_err_message, pon_err_occurred, pon_err_set, thread_state_lock};

unsafe extern "C" {
    fn pon_module_main() -> *mut PyObject;
    fn pon_aot_init_names();
    fn pon_aot_init_modules();
}

/// Seeds one AoT-embedded name into the runtime interner.
///
/// AoT object code embeds compact name ids allocated while building the object.
/// The generated `pon_aot_init_names` function replays those strings through
/// this helper before normal runtime initialization registers builtins.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_aot_intern_name(name: *const u8, len: usize) {
    if name.is_null() {
        if len == 0 {
            let _ = crate::intern::intern("");
        } else {
            pon_err_set("AoT name initializer received a null name pointer");
        }
        return;
    }

    let bytes = unsafe { std::slice::from_raw_parts(name, len) };
    match std::str::from_utf8(bytes) {
        Ok(name) => {
            let _ = crate::intern::intern(name);
        }
        Err(_) => pon_err_set("AoT name initializer received invalid UTF-8"),
    }
}

/// Runs the compiled AoT module with process-style `argc`/`argv` inputs.
///
/// The generated executable owns the tiny platform `main` trampoline; all runtime
/// sequencing stays here so JIT entry remains unchanged. Failures follow the
/// NULL-sentinel discipline internally, are reported once as uncaught exceptions,
/// and become process exit code `1`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_aot_entry(argc: i32, argv: *const *const u8) -> i32 {
    match catch_unwind(AssertUnwindSafe(|| unsafe { pon_aot_entry_impl(argc, argv) })) {
        Ok(code) => code,
        Err(_) => {
            pon_err_set("AoT entry panicked");
            unsafe { pon_err_report_uncaught() };
            let _ = unsafe { crate::sys::pon_io_flush_std() };
            1
        }
    }
}

unsafe fn pon_aot_entry_impl(argc: i32, argv: *const *const u8) -> i32 {
    let mut stack_base_marker = 0usize;

    unsafe { pon_aot_init_names() };
    if pon_err_occurred() {
        unsafe { pon_err_report_uncaught() };
        let _ = unsafe { crate::sys::pon_io_flush_std() };
        return 1;
    }

    // Register AoT-embedded module bodies before runtime init so the first
    // `import` executed by the module main already sees the full registry.
    unsafe { pon_aot_init_modules() };
    if pon_err_occurred() {
        unsafe { pon_err_report_uncaught() };
        let _ = unsafe { crate::sys::pon_io_flush_std() };
        return 1;
    }

    if unsafe { crate::abi::pon_runtime_init() } != 0 {
        unsafe { pon_err_report_uncaught() };
        let _ = unsafe { crate::sys::pon_io_flush_std() };
        return 1;
    }

    capture_stack_base(ptr::addr_of_mut!(stack_base_marker).cast::<u8>());

    if unsafe { crate::sys::pon_sys_set_argv(argc, argv) } != 0 {
        unsafe { pon_err_report_uncaught() };
        let _ = unsafe { crate::sys::pon_io_flush_std() };
        return 1;
    }

    // Mirror the JIT driver (pon-cli `run_file_inner`): top-level code executes
    // inside a `__main__` module-execution context. The globals()/compiled-slot
    // coherence hooks (`sync_globals_dict_set`, `sync_global_store_for_active_module`)
    // all key on `active_module_name_id()`; without this context a dict write
    // through globals() never lands in the store compiled loads consult.
    let main_context = crate::import::install_module("__main__", []).and_then(|_| crate::import::begin_module_execution("__main__"));
    if let Err(message) = main_context {
        pon_err_set(&message);
        unsafe { pon_err_report_uncaught() };
        let _ = unsafe { crate::sys::pon_io_flush_std() };
        return 1;
    }

    let module_result = unsafe { pon_module_main() };
    crate::import::end_module_execution("__main__");
    let mut exit_code = if module_result.is_null() {
        if !pon_err_occurred() {
            pon_err_set("module main returned NULL without setting an exception");
        }
        unsafe { pon_err_report_uncaught() };
        1
    } else {
        0
    };

    if unsafe { crate::sys::pon_io_flush_std() } != 0 && exit_code == 0 {
        exit_code = 1;
    }
    exit_code
}

fn capture_stack_base(base: *mut u8) {
    thread_state_lock().stack_base = base;
    pon_gc::set_external_stack_base(base);
}

/// Captures a conservative-stack upper boundary for runtimes entering generated code.
///
/// `pon_aot_entry` records a pointer from its own frame so collection scans cover
/// values live in the real process-entry frame. This exported helper is available
/// for non-standard embedders; it records a pointer from the helper frame and
/// returns `0` rather than exposing that frame-local address to callers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_threadstate_capture_stack_base() -> i32 {
    let mut stack_base_marker = 0usize;
    let base = ptr::addr_of_mut!(stack_base_marker).cast::<u8>();
    capture_stack_base(base);
    0
}

/// Prints the current NULL-sentinel exception as an uncaught process error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_err_report_uncaught() -> i32 {
    match catch_unwind(AssertUnwindSafe(report_uncaught)) {
        Ok(Ok(())) => 0,
        Ok(Err(message)) => {
            pon_err_set(message);
            -1
        }
        Err(_) => {
            pon_err_set("uncaught exception reporting panicked");
            -1
        }
    }
}

fn report_uncaught() -> Result<(), String> {
    let message = pon_err_message().unwrap_or_else(|| "uncaught exception without diagnostic".to_owned());
    let mut stderr = io::stderr().lock();
    writeln!(stderr, "Traceback (most recent call last):").map_err(|error| error.to_string())?;
    writeln!(stderr, "{message}").map_err(|error| error.to_string())?;
    stderr.flush().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use std::ptr;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::object::PyObject;
    use crate::thread_state::test_state_lock;

    use super::pon_aot_entry_impl;

    static MODULE_MAIN_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[unsafe(no_mangle)]
    unsafe extern "C" fn pon_module_main() -> *mut PyObject {
        MODULE_MAIN_CALLS.fetch_add(1, Ordering::SeqCst);
        ptr::dangling_mut::<PyObject>()
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn pon_aot_init_names() {}

    #[unsafe(no_mangle)]
    unsafe extern "C" fn pon_aot_init_modules() {}

    #[test]
    fn aot_entry_invokes_zero_arg_module_main_once() {
        type ModuleMain = unsafe extern "C" fn() -> *mut PyObject;
        let _: ModuleMain = super::pon_module_main;

        let _guard = test_state_lock();
        // Parallel tests share one process-global error slot: drop any stale
        // pending error another test left behind so the entry path's
        // `pon_err_occurred()` gates see the fresh-process state they assume.
        crate::thread_state::pon_err_clear();
        MODULE_MAIN_CALLS.store(0, Ordering::SeqCst);

        let exit_code = unsafe { pon_aot_entry_impl(0, ptr::null()) };

        assert_eq!(exit_code, 0);
        assert_eq!(MODULE_MAIN_CALLS.load(Ordering::SeqCst), 1);
    }
}
