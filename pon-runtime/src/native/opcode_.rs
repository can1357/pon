//! Native `_opcode` seed (test.support chain: `test.support`/`doctest` ->
//! `inspect` -> `dis` -> `opcode` -> `_opcode`).
//!
//! CPython's `_opcode` is a C view over the interpreter's opcode metadata
//! tables (`Modules/_opcode.c`).  pon lowers Python source to its own IR and
//! has no CPython bytecode — a permanent divergence, so real bytecode
//! introspection is out of scope here (the `dis`/`test__opcode` family is an
//! interpreter-internals exclusion candidate).  This module serves exactly
//! what module scope of `Lib/opcode.py`, `Lib/dis.py`, and
//! `Lib/test/support/__init__.py` evaluates:
//!
//! * `has_arg`/`has_const`/`has_name`/`has_jump`/`has_free`/`has_local`/
//!   `has_exc` are *called* at `import opcode` time, once per op in
//!   `opmap.values()`.  They return `False`: no opcode carries bytecode
//!   properties in pon, so the documented `dis.hasarg`/`hasconst`/... lists
//!   come out empty (deliberate divergence, noted above).
//! * `get_intrinsic1_descs`/`get_intrinsic2_descs`/`get_special_method_names`
//!   /`get_nb_ops` are called at import and return fresh empty lists (pon has
//!   no intrinsics or binary-op specialization tables).
//! * `get_specialization_stats` returns `None`, exactly like a CPython build
//!   without `Py_STATS`.
//! * `stack_effect` (bound by name in `opcode.py`), `get_executor` (bound by
//!   name in `dis.py`), and `is_valid` exist so the imports succeed, and
//!   raise a typed `NotImplementedError` naming the gap when actually called.
//! * `ENABLE_SPECIALIZATION`/`ENABLE_SPECIALIZATION_FT` are `0`: pon never
//!   specializes, so `test.support.requires_specialization{,_ft}` skips.
//!
//! Everything real about opcodes (`cmp_op`, `opmap`, `opname`,
//! `HAVE_ARGUMENT`, `EXTENDED_ARG`, ...) comes from the vendored pure-Python
//! `Lib/_opcode_metadata.py` via `opcode.py` and needs nothing from here.

use crate::abi;
use crate::intern::intern;
use crate::object::PyObject;
use crate::types::exc::ExceptionKind;

use super::builtins_mod::{alloc_list, VARIADIC_ARITY};
use super::install_module;

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

/// The seven opcode-category predicates, *called* at `import opcode` time
/// over every `opmap` value.  pon executes no CPython bytecode, so every
/// probe answers `False`.
macro_rules! category_predicate_fns {
    ($(($entry:ident, $name:literal)),* $(,)?) => {
        $(
            unsafe extern "C" fn $entry(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
                if argc != 1 {
                    return abi::exc::raise_kind_error_text(
                        ExceptionKind::TypeError,
                        &format!(concat!("_opcode.", $name, " expected 1 argument, got {}"), argc),
                    );
                }
                // SAFETY: Bool constructor returns the shared singleton.
                unsafe { abi::pon_const_bool(0) }
            }
        )*
    };
}

category_predicate_fns!(
    (has_arg_entry, "has_arg"),
    (has_const_entry, "has_const"),
    (has_name_entry, "has_name"),
    (has_jump_entry, "has_jump"),
    (has_free_entry, "has_free"),
    (has_local_entry, "has_local"),
    (has_exc_entry, "has_exc"),
);

/// Zero-argument table getters, *called* at `import opcode` time.  CPython
/// returns fresh lists describing interpreter tables pon does not have; the
/// honest pon answer is a fresh empty list.
macro_rules! empty_table_fns {
    ($(($entry:ident, $name:literal)),* $(,)?) => {
        $(
            unsafe extern "C" fn $entry(_argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
                if argc != 0 {
                    return abi::exc::raise_kind_error_text(
                        ExceptionKind::TypeError,
                        &format!(concat!("_opcode.", $name, " expected 0 arguments, got {}"), argc),
                    );
                }
                alloc_list(Vec::new())
            }
        )*
    };
}

empty_table_fns!(
    (get_nb_ops_entry, "get_nb_ops"),
    (get_intrinsic1_descs_entry, "get_intrinsic1_descs"),
    (get_intrinsic2_descs_entry, "get_intrinsic2_descs"),
    (get_special_method_names_entry, "get_special_method_names"),
);

/// Entry points whose real semantics require CPython bytecode (stack
/// metrics, opcode-table validity, tier-2 executors).  Bound by name at
/// import — `opcode.py` binds `stack_effect`, `dis.py` binds `get_executor`
/// — and loud at the exact call site.
macro_rules! bytecode_stub_fns {
    ($(($entry:ident, $name:literal)),* $(,)?) => {
        $(
            unsafe extern "C" fn $entry(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
                abi::exc::raise_kind_error_text(
                    ExceptionKind::NotImplementedError,
                    concat!("_opcode.", $name, " is not implemented in pon (no CPython bytecode; see native/opcode_.rs)"),
                )
            }
        )*
    };
}

bytecode_stub_fns!(
    (stack_effect_entry, "stack_effect"),
    (is_valid_entry, "is_valid"),
    (get_executor_entry, "get_executor"),
);

/// `get_specialization_stats()`: a CPython build without `Py_STATS` returns
/// `None`; so does pon (it collects no specialization stats).
unsafe extern "C" fn get_specialization_stats_entry(
    _argv: *mut *mut PyObject,
    argc: usize,
) -> *mut PyObject {
    if argc != 0 {
        return abi::exc::raise_kind_error_text(
            ExceptionKind::TypeError,
            &format!("_opcode.get_specialization_stats expected 0 arguments, got {argc}"),
        );
    }
    // SAFETY: Singleton accessor.
    unsafe { abi::pon_none() }
}

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let name = "_opcode";
    // SAFETY: Runtime allocation helper; NULL is checked below.
    let name_obj = unsafe { abi::pon_const_str(name.as_ptr(), name.len()) };
    if name_obj.is_null() {
        return Err("failed to allocate _opcode.__name__".to_owned());
    }
    let mut attrs: Vec<(u32, *mut PyObject)> = vec![(intern("__name__"), name_obj)];
    // pon never specializes (it has no bytecode to specialize): both flags
    // are 0, which makes `test.support.requires_specialization{,_ft}` skip.
    for const_name in ["ENABLE_SPECIALIZATION", "ENABLE_SPECIALIZATION_FT"] {
        // SAFETY: Int constant allocator; NULL on failure with the error set.
        let zero = unsafe { abi::pon_const_int(0) };
        if zero.is_null() {
            return Err(format!("failed to allocate _opcode.{const_name}"));
        }
        attrs.push((intern(const_name), zero));
    }
    // Method-table order mirrors CPython's `Modules/_opcode.c`.
    for (fn_name, entry) in [
        ("stack_effect", stack_effect_entry as BuiltinFn),
        ("is_valid", is_valid_entry),
        ("has_arg", has_arg_entry),
        ("has_const", has_const_entry),
        ("has_name", has_name_entry),
        ("has_jump", has_jump_entry),
        ("has_free", has_free_entry),
        ("has_local", has_local_entry),
        ("has_exc", has_exc_entry),
        ("get_specialization_stats", get_specialization_stats_entry),
        ("get_nb_ops", get_nb_ops_entry),
        ("get_intrinsic1_descs", get_intrinsic1_descs_entry),
        ("get_intrinsic2_descs", get_intrinsic2_descs_entry),
        ("get_special_method_names", get_special_method_names_entry),
        ("get_executor", get_executor_entry),
    ] {
        // SAFETY: `entry` is a live builtin entry point with the runtime
        // calling convention.
        let function =
            unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(fn_name)) };
        if function.is_null() {
            return Err(format!("failed to allocate _opcode.{fn_name}"));
        }
        attrs.push((intern(fn_name), function));
    }
    install_module(name, attrs)
}
