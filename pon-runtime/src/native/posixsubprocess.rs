//! Native `_posixsubprocess` seed: import surface only.
//!
//! CPython's `_posixsubprocess` is the C fork/exec helper
//! (`Modules/_posixsubprocess.c`); `Lib/subprocess.py` imports it
//! unconditionally on POSIX hosts (`from _posixsubprocess import fork_exec`
//! under the `_can_fork_exec` branch) but only *calls* it from
//! `Popen._execute_child`.  The import chain that matters here is
//! `asyncio.base_events -> subprocess`: asyncio needs `import subprocess` to
//! succeed at module scope long before anything spawns a process.  pon does
//! not wire fork/exec to the host yet, so `fork_exec` exists with the real
//! name and raises a typed `NotImplementedError` when actually invoked —
//! loud at the exact call site instead of a silent wrong result.  The real
//! module exports nothing else; `subprocess.py` reads no constants from it.

use crate::abi::{pon_const_str, pon_make_function};
use crate::intern::intern;
use crate::object::PyObject;
use crate::types::exc::ExceptionKind;

use super::install_module;

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let name = "_posixsubprocess";
    // SAFETY: Runtime allocation helper; NULL is checked below.
    let name_obj = unsafe { pon_const_str(name.as_ptr(), name.len()) };
    if name_obj.is_null() {
        return Err("failed to allocate _posixsubprocess.__name__".to_owned());
    }
    let fork_exec_name = intern("fork_exec");
    // SAFETY: Live builtin entry point with the runtime calling convention.
    let fork_exec = unsafe {
        pon_make_function(
            posixsubprocess_fork_exec as *const u8,
            crate::builtins::variadic_arity(),
            fork_exec_name,
        )
    };
    if fork_exec.is_null() {
        return Err("failed to allocate _posixsubprocess.fork_exec".to_owned());
    }
    install_module(
        name,
        vec![(intern("__name__"), name_obj), (fork_exec_name, fork_exec)],
    )
}

/// `fork_exec(args, executable_list, close_fds, ...)`: the one entry point
/// the real module exports.  Honest refusal until pon wires process spawning
/// to the host; `subprocess.Popen` surfaces this from `_execute_child`.
unsafe extern "C" fn posixsubprocess_fork_exec(
    _argv: *mut *mut PyObject,
    _argc: usize,
) -> *mut PyObject {
    crate::abi::exc::raise_kind_error_text(
        ExceptionKind::NotImplementedError,
        "_posixsubprocess.fork_exec is not implemented in pon: process spawning is not wired to the host yet (see native/posixsubprocess.rs)",
    )
}
