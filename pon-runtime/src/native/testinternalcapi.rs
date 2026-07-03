//! Native `_testinternalcapi` shim (owner-sanctioned, ledger-triage wave 6).
//!
//! CPython's `_testinternalcapi` is a C test extension over interpreter
//! internals (`Modules/_testinternalcapi.c`).  Almost all of it probes
//! machinery pon does not have (specializer counters, uop optimizer, TLBC,
//! cross-interpreter data) and the units doing so are excluded
//! (`interpreter-internals` rows in `pon-conformance/exclusions.toml`).  One
//! meaningful unit dies on the module's absence: `test.test_class:859` runs
//! `from _testinternalcapi import has_inline_values` *unguarded* at module
//! scope — a raw `ModuleNotFoundError` that kills 858 preceding lines of
//! core class-model coverage (`local://exclusion-proposal.md` §Gray).
//!
//! Surface policy (minimal honest, J0.4): exactly the name that hard
//! consumer imports.  Every other `_testinternalcapi` name is deliberately
//! absent — the consuming families are excluded, and fabricating
//! interpreter-internals answers here would corrupt the meter.  An
//! un-shimmed access fails loudly as `AttributeError` naming this module.
//!
//! # `has_inline_values` semantics under pon's instance layout
//!
//! CPython's implementation returns `True` iff the object's type has
//! `Py_TPFLAGS_MANAGED_DICT` **and** the per-object inline-values block is
//! still `valid` — i.e. attribute values currently live in storage owned by
//! the object itself rather than in a detached dict (detachment happens on
//! `del obj.__dict__` or when the attribute count outgrows the inline
//! block).
//!
//! pon's analogue of "managed dict" is the ordinary heap-instance layout:
//! [`crate::types::type_::PyHeapInstance`] owns its attribute namespace
//! outright (`dict: *mut PyClassDict`, lazily materialized, handed out only
//! as live views), and `construct_class` marks types whose instances carry
//! one with `tp_dictoffset != 0` and stamps `gc_type_id =
//! TYPE_ID_HEAP_INSTANCE` (payload subclasses — `class C(int)` — share the
//! prefix and the stamp, matching CPython where such types are also
//! managed-dict).  Crucially, pon has **no detach transition at all**: the
//! namespace never spills to an external dict on growth, and `del
//! obj.__dict__` is refused (`member_descriptor_set`: "__dict__ attribute is
//! read-only"), so the per-object `valid` component of CPython's predicate
//! is constantly true whenever the type-level component holds.  The honest
//! answer is therefore a pure type-level check:
//!
//! * heap object whose type is stamped `TYPE_ID_HEAP_INSTANCE` with
//!   `tp_dictoffset != 0` → `True` (attribute values are in instance-owned
//!   storage — precisely the state CPython calls "has inline values");
//! * everything else — tagged immediates, builtins, modules, slot-only
//!   instances (`tp_dictoffset == 0`), boxed-exception layouts (their
//!   `gc_type_id` is the exception family; CPython exception types inherit
//!   `tp_dictoffset` from `BaseException` and are *not* managed-dict) →
//!   `False`.
//!
//! Known honest divergences in `test_class.TestInlineValues`:
//! `test_many_attributes` expects `False` after 100 attribute stores (pon
//! never spills, so it stays `True`), and `test_has_inline_values` performs
//! `del c.__dict__` (pon refuses `__dict__` deletion before this function is
//! even consulted).  Both surface as honest per-test vectors instead of an
//! import-time death of the whole unit.

use crate::abi;
use crate::intern::intern;
use crate::object::{PyObject, PyType};
use crate::types::exc::ExceptionKind;
use crate::types::type_::TYPE_ID_HEAP_INSTANCE;

use super::builtins_mod::VARIADIC_ARITY;
use super::install_module;

/// Type of `object`, or NULL for NULL/tagged immediates (module-local copy
/// of the runtime-wide helper convention; immediates carry no
/// dereferenceable type).
unsafe fn object_type(object: *mut PyObject) -> *mut PyType {
    if object.is_null() || !crate::tag::is_heap(object) {
        core::ptr::null_mut()
    } else {
        unsafe { (*object).ob_type.cast_mut() }
    }
}

/// `has_inline_values(obj)`: whether `obj`'s attribute values live in
/// instance-owned managed storage (see the module docs for the full mapping
/// onto CPython's inline-values state).
unsafe extern "C" fn has_inline_values_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 1 {
        return abi::exc::raise_kind_error_text(
            ExceptionKind::TypeError,
            &format!("_testinternalcapi.has_inline_values expected 1 argument, got {argc}"),
        );
    }
    if argv.is_null() {
        return abi::exc::raise_kind_error_text(
            ExceptionKind::TypeError,
            "_testinternalcapi.has_inline_values received a NULL argument buffer",
        );
    }
    // SAFETY: `argv` holds `argc` live argument pointers (checked above).
    let object = unsafe { *argv };
    let ty = unsafe { object_type(object) };
    let managed = !ty.is_null()
        && unsafe { (*ty).gc_type_id } == TYPE_ID_HEAP_INSTANCE.0 as usize
        && unsafe { (*ty).tp_dictoffset } != 0;
    // SAFETY: Bool constructor returns the shared singleton.
    unsafe { abi::number::pon_const_bool(i32::from(managed)) }
}

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let name = "_testinternalcapi";
    // SAFETY: Runtime allocation helper; NULL is checked below.
    let name_obj = unsafe { abi::pon_const_str(name.as_ptr(), name.len()) };
    if name_obj.is_null() {
        return Err("failed to allocate _testinternalcapi.__name__".to_owned());
    }
    let doc = "pon-native shim for CPython's _testinternalcapi C test extension: \
               serves has_inline_values (mapped honestly onto pon's instance \
               layout) for test_class; every other internals probe is \
               deliberately absent — see native/testinternalcapi.rs.";
    // SAFETY: Runtime allocation helper; NULL is checked below.
    let doc_obj = unsafe { abi::pon_const_str(doc.as_ptr(), doc.len()) };
    if doc_obj.is_null() {
        return Err("failed to allocate _testinternalcapi.__doc__".to_owned());
    }
    let mut attrs: Vec<(u32, *mut PyObject)> = vec![(intern("__name__"), name_obj), (intern("__doc__"), doc_obj)];
    // SAFETY: `has_inline_values_entry` is a live builtin entry point with
    // the runtime calling convention.
    let function = unsafe {
        abi::pon_make_function(
            has_inline_values_entry as *const u8,
            VARIADIC_ARITY,
            intern("has_inline_values"),
        )
    };
    if function.is_null() {
        return Err("failed to allocate _testinternalcapi.has_inline_values".to_owned());
    }
    attrs.push((intern("has_inline_values"), function));
    install_module(name, attrs)
}
