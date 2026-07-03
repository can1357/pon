//! CPython `_sre` seed: the isolated bytecode VM in [`vm`] plus the runtime
//! wrapper that registers it as the native `_sre` module (HANDOFF Wave 2).
//!
//! `vm.rs` stays dependency-free (std only): `tests/sre_vm.rs` compiles it
//! standalone via `#[path]` against the vendored `re._compiler` fixtures.
//! Everything runtime-facing lives here: the module factory consumed by
//! [`super::NATIVE_MODULES`], the `_sre.compile` entry point, and the
//! `re.Pattern` / `re.Match` object wrappers used by the vendored `Lib/re`.

// `vm` keeps its full public surface for the standalone `tests/sre_vm.rs`
// include; only a subset is exercised from this in-crate wrapper.
#[allow(dead_code)]
mod vm;

use core::ffi::c_int;
use std::collections::BTreeMap;
use std::ptr;
use std::sync::LazyLock;

use num_traits::ToPrimitive;

use crate::abi::{self, pon_get_iter, pon_iter_next};
use crate::intern::intern;
use crate::object::{PyMappingMethods, PyObject, PyObjectHeader, PyType};
use crate::thread_state::{pon_err_clear, pon_err_occurred, pon_err_set};
use crate::types::type_::unicode_text;
use crate::types::{bool_, bytes_};

use super::builtins_mod::{alloc_list, alloc_tuple, repr_text, VARIADIC_ARITY};
use super::install_module;

/// CPython 3.14 `_sre.MAXGROUPS` (`(1 << 30) - 1`).
const MAXGROUPS: i64 = 1_073_741_823;

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let mut attrs = vec![
        string_attr("__name__", "_sre")?,
        int_attr("MAGIC", i64::from(vm::MAGIC))?,
        int_attr("CODESIZE", vm::CODESIZE as i64)?,
        int_attr("MAXREPEAT", i64::from(vm::MAXREPEAT))?,
        int_attr("MAXGROUPS", MAXGROUPS)?,
    ];
    let functions: [(&str, BuiltinFn); 7] = [
        ("compile", sre_compile),
        ("getcodesize", sre_getcodesize),
        ("ascii_iscased", sre_ascii_iscased),
        ("unicode_iscased", sre_unicode_iscased),
        ("ascii_tolower", sre_ascii_tolower),
        ("unicode_tolower", sre_unicode_tolower),
        ("template", sre_template),
    ];
    for (name, entry) in functions {
        attrs.push(function_attr(name, entry)?);
    }
    install_module("_sre", attrs)
}

fn string_attr(name: &str, value: &str) -> Result<(u32, *mut PyObject), String> {
    // SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
    let object = unsafe { abi::pon_const_str(value.as_ptr(), value.len()) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate _sre.{name}"))
}

fn int_attr(name: &str, value: i64) -> Result<(u32, *mut PyObject), String> {
    // SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
    let object = unsafe { abi::pon_const_int(value) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate _sre.{name}"))
}

fn function_attr(name: &str, entry: BuiltinFn) -> Result<(u32, *mut PyObject), String> {
    // SAFETY: `entry` is a live builtin entry point with the runtime calling convention.
    let object = unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate _sre.{name}"))
}

// ---------------------------------------------------------------------------
// Object layouts

#[repr(C)]
struct SrePattern {
    ob_base: PyObjectHeader,
    pattern: vm::Pattern,
    /// Original pattern object (echoed as `Pattern.pattern`).
    pattern_obj: *mut PyObject,
    /// Original groupindex dict (echoed as `Pattern.groupindex`).
    groupindex_obj: *mut PyObject,
    /// Flags exactly as passed to `_sre.compile` (echoed as `Pattern.flags`).
    flags: i64,
}

#[repr(C)]
struct SreMatch {
    ob_base: PyObjectHeader,
    matched: vm::Match,
    /// The owning `re.Pattern` object (echoed as `Match.re`).
    pattern_obj: *mut PyObject,
    /// Original subject object (echoed as `Match.string`).
    string_obj: *mut PyObject,
}

#[repr(C)]
struct SreIterator {
    ob_base: PyObjectHeader,
    items: Vec<*mut PyObject>,
    index: usize,
}

static PATTERN_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = Box::new(PyType::new(ptr::null(), "re.Pattern", std::mem::size_of::<SrePattern>()));
    ty.tp_getattro = Some(pattern_getattro);
    ty.tp_repr = Some(pattern_repr);
    ty.tp_str = Some(pattern_repr);
    ty.tp_hash = Some(identity_hash);
    ty.tp_bool = Some(always_true);
    Box::into_raw(ty) as usize
});

static MATCH_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = Box::new(PyType::new(ptr::null(), "re.Match", std::mem::size_of::<SreMatch>()));
    ty.tp_getattro = Some(match_getattro);
    ty.tp_repr = Some(match_repr);
    ty.tp_str = Some(match_repr);
    ty.tp_hash = Some(identity_hash);
    ty.tp_bool = Some(always_true);
    // CPython `match_getitem`: `m[group]` is `m.group(group)` for a single
    // int or str selector.
    ty.tp_as_mapping = Box::into_raw(Box::new(PyMappingMethods {
        mp_subscript: Some(match_subscript),
        ..PyMappingMethods::EMPTY
    }));
    Box::into_raw(ty) as usize
});

static ITERATOR_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = Box::new(PyType::new(
        ptr::null(),
        "callable_iterator",
        std::mem::size_of::<SreIterator>(),
    ));
    ty.tp_iter = Some(identity_slot);
    ty.tp_iternext = Some(iterator_next_slot);
    ty.tp_hash = Some(identity_hash);
    ty.tp_bool = Some(always_true);
    Box::into_raw(ty) as usize
});

fn pattern_type() -> *mut PyType {
    *PATTERN_TYPE as *mut PyType
}

fn match_type() -> *mut PyType {
    *MATCH_TYPE as *mut PyType
}

fn iterator_type() -> *mut PyType {
    *ITERATOR_TYPE as *mut PyType
}

fn alloc_pattern(
    pattern: vm::Pattern,
    pattern_obj: *mut PyObject,
    groupindex_obj: *mut PyObject,
    flags: i64,
) -> *mut PyObject {
    Box::into_raw(Box::new(SrePattern {
        ob_base: PyObjectHeader::new(pattern_type()),
        pattern,
        pattern_obj,
        groupindex_obj,
        flags,
    }))
    .cast::<PyObject>()
}

fn alloc_match(matched: vm::Match, pattern_obj: *mut PyObject, string_obj: *mut PyObject) -> *mut PyObject {
    Box::into_raw(Box::new(SreMatch {
        ob_base: PyObjectHeader::new(match_type()),
        matched,
        pattern_obj,
        string_obj,
    }))
    .cast::<PyObject>()
}

fn alloc_iterator(items: Vec<*mut PyObject>) -> *mut PyObject {
    Box::into_raw(Box::new(SreIterator {
        ob_base: PyObjectHeader::new(iterator_type()),
        items,
        index: 0,
    }))
    .cast::<PyObject>()
}

unsafe fn as_pattern<'a>(object: *mut PyObject) -> Option<&'a mut SrePattern> {
    let object = untag(object);
    if object.is_null() {
        return None;
    }
    // SAFETY: NULL was rejected above; the type check gates the downcast.
    (unsafe { (*object).ob_type } == pattern_type().cast_const())
        .then(|| unsafe { &mut *object.cast::<SrePattern>() })
}

unsafe fn as_match<'a>(object: *mut PyObject) -> Option<&'a mut SreMatch> {
    let object = untag(object);
    if object.is_null() {
        return None;
    }
    // SAFETY: NULL was rejected above; the type check gates the downcast.
    (unsafe { (*object).ob_type } == match_type().cast_const())
        .then(|| unsafe { &mut *object.cast::<SreMatch>() })
}

unsafe fn as_iterator<'a>(object: *mut PyObject) -> Option<&'a mut SreIterator> {
    let object = untag(object);
    if object.is_null() {
        return None;
    }
    // SAFETY: NULL was rejected above; the type check gates the downcast.
    (unsafe { (*object).ob_type } == iterator_type().cast_const())
        .then(|| unsafe { &mut *object.cast::<SreIterator>() })
}

// ---------------------------------------------------------------------------
// Shared slot implementations

unsafe extern "C" fn identity_hash(object: *mut PyObject) -> isize {
    object.addr() as isize
}

unsafe extern "C" fn always_true(_object: *mut PyObject) -> c_int {
    1
}

unsafe extern "C" fn identity_slot(object: *mut PyObject) -> *mut PyObject {
    object
}

unsafe extern "C" fn iterator_next_slot(object: *mut PyObject) -> *mut PyObject {
    let Some(iterator) = (unsafe { as_iterator(object) }) else {
        return fail("re iterator receiver is invalid");
    };
    if iterator.index >= iterator.items.len() {
        return unsafe { abi::pon_raise_stop_iteration(ptr::null_mut()) };
    }
    let value = iterator.items[iterator.index];
    iterator.index += 1;
    value
}

// ---------------------------------------------------------------------------
// Small conversion helpers

fn untag(object: *mut PyObject) -> *mut PyObject {
    crate::tag::untag_arg(object)
}

fn fail(message: impl Into<String>) -> *mut PyObject {
    pon_err_set(message);
    ptr::null_mut()
}

fn none() -> *mut PyObject {
    // SAFETY: Singleton accessor.
    unsafe { abi::pon_none() }
}

fn alloc_str_object(text: &str) -> *mut PyObject {
    // SAFETY: Runtime allocation helper; NULL on failure with the error set.
    unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

fn alloc_bytes_object(bytes: &[u8]) -> *mut PyObject {
    bytes_::boxed_bytes(bytes).cast::<PyObject>()
}

fn alloc_int_object(value: i64) -> *mut PyObject {
    // SAFETY: Runtime allocation helper; NULL on failure with the error set.
    unsafe { abi::pon_const_int(value) }
}

unsafe fn arg_slice<'a>(argv: *mut *mut PyObject, argc: usize) -> Option<&'a [*mut PyObject]> {
    if argc == 0 {
        Some(&[])
    } else if argv.is_null() {
        None
    } else {
        // SAFETY: The caller passed `argc` live argument slots.
        Some(unsafe { std::slice::from_raw_parts(argv, argc) })
    }
}

fn to_i64(object: *mut PyObject) -> Option<i64> {
    let object = untag(object);
    if object.is_null() {
        return None;
    }
    // SAFETY: `object` is heap-or-NULL after untagging and NULL was rejected.
    unsafe { crate::types::int::to_bigint_including_bool(object) }.and_then(|value| value.to_i64())
}

/// CPython `%.<limit>R` formatting (`_sre.c` uses `%.200R` for the pattern in
/// `Pattern.__repr__` and `%.50R` for the matched text in `Match.__repr__`):
/// the argument's repr clipped to at most `limit` code points.
fn clipped_repr(object: *mut PyObject, limit: usize) -> String {
    let text = repr_text(object);
    match text.char_indices().nth(limit) {
        Some((offset, _)) => text[..offset].to_owned(),
        None => text,
    }
}

fn collect_iterable(object: *mut PyObject) -> Result<Vec<*mut PyObject>, String> {
    // SAFETY: `pon_get_iter`/`pon_iter_next` self-normalize their arguments.
    let iter = unsafe { pon_get_iter(object, ptr::null_mut()) };
    if iter.is_null() {
        return Err("object is not iterable".to_owned());
    }
    let mut items = Vec::new();
    loop {
        // SAFETY: `iter` is the live iterator obtained above.
        let value = unsafe { pon_iter_next(iter, ptr::null_mut()) };
        if value.is_null() {
            if pon_err_occurred() {
                pon_err_clear();
            }
            break;
        }
        items.push(untag(value));
    }
    Ok(items)
}

// ---------------------------------------------------------------------------
// Subject handling (str spans are code-point indices, bytes spans are offsets)

enum Subject {
    Str { text: String, offsets: Vec<usize> },
    Bytes { data: Vec<u8> },
}

impl Subject {
    fn from_object(object: *mut PyObject) -> Option<Subject> {
        let object = untag(object);
        // SAFETY: heap-or-NULL after untagging; accessors reject NULL.
        if let Some(text) = unsafe { unicode_text(object) } {
            let mut offsets: Vec<usize> = text.char_indices().map(|(index, _)| index).collect();
            offsets.push(text.len());
            return Some(Subject::Str { text: text.to_owned(), offsets });
        }
        if !object.is_null() && bytes_::is_bytes_type(unsafe { (*object).ob_type }) {
            // SAFETY: The type check above guarantees a live `PyBytes`.
            let data = unsafe { (*object.cast::<bytes_::PyBytes>()).as_slice() }.to_vec();
            return Some(Subject::Bytes { data });
        }
        None
    }

    fn units(&self) -> usize {
        match self {
            Subject::Str { offsets, .. } => offsets.len() - 1,
            Subject::Bytes { data } => data.len(),
        }
    }

    fn is_bytes(&self) -> bool {
        matches!(self, Subject::Bytes { .. })
    }

    fn slice_bytes(&self, start: usize, end: usize) -> &[u8] {
        let limit = self.units();
        let start = start.min(limit);
        let end = end.clamp(start, limit);
        match self {
            Subject::Str { text, offsets } => &text.as_bytes()[offsets[start]..offsets[end]],
            Subject::Bytes { data } => &data[start..end],
        }
    }

    fn make_object(&self, payload: &[u8]) -> *mut PyObject {
        if self.is_bytes() {
            alloc_bytes_object(payload)
        } else {
            alloc_str_object(&String::from_utf8_lossy(payload))
        }
    }
}

fn matched_value_object(value: &vm::MatchedValue) -> *mut PyObject {
    match value {
        vm::MatchedValue::Str(text) => alloc_str_object(text),
        vm::MatchedValue::Bytes(bytes) => alloc_bytes_object(bytes),
    }
}

// ---------------------------------------------------------------------------
// Module-level functions

unsafe extern "C" fn sre_compile(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return fail("_sre.compile() received a null argv pointer");
    };
    if args.len() != 6 {
        return fail(format!("_sre.compile() expected 6 arguments, got {}", args.len()));
    }
    let pattern_obj = untag(args[0]);
    let Some(flags) = to_i64(args[1]) else {
        return fail("_sre.compile() flags must be an int");
    };
    let code_items = match collect_iterable(untag(args[2])) {
        Ok(items) => items,
        Err(message) => return fail(format!("_sre.compile() code: {message}")),
    };
    let Some(groups) = to_i64(args[3]).and_then(|value| usize::try_from(value).ok()) else {
        return fail("_sre.compile() groups must be a non-negative int");
    };
    let groupindex_obj = untag(args[4]);
    let indexgroup_items = match collect_iterable(untag(args[5])) {
        Ok(items) => items,
        Err(message) => return fail(format!("_sre.compile() indexgroup: {message}")),
    };

    let pattern_text = match unsafe { unicode_text(pattern_obj) } {
        Some(text) => vm::PatternText::Str(text.to_owned()),
        None if !pattern_obj.is_null() && bytes_::is_bytes_type(unsafe { (*pattern_obj).ob_type }) => {
            // SAFETY: The type check above guarantees a live `PyBytes`.
            vm::PatternText::Bytes(unsafe { (*pattern_obj.cast::<bytes_::PyBytes>()).as_slice() }.to_vec())
        }
        None => vm::PatternText::Unknown,
    };

    let mut code = Vec::with_capacity(code_items.len());
    for item in code_items {
        let Some(word) = to_i64(item).and_then(|value| u32::try_from(value).ok()) else {
            return fail("_sre.compile() code must be a sequence of 32-bit ints");
        };
        code.push(word);
    }

    let mut indexgroup = Vec::with_capacity(indexgroup_items.len());
    let mut groupindex = BTreeMap::new();
    for (index, item) in indexgroup_items.into_iter().enumerate() {
        // SAFETY: items were untagged by `collect_iterable`.
        match unsafe { unicode_text(item) } {
            Some(name) => {
                groupindex.insert(name.to_owned(), index);
                indexgroup.push(Some(name.to_owned()));
            }
            None => indexgroup.push(None),
        }
    }

    match vm::compile(pattern_text, flags as u32, code, groups, groupindex, indexgroup) {
        Ok(pattern) => alloc_pattern(pattern, pattern_obj, groupindex_obj, flags),
        Err(error) => fail(format!("_sre.compile() rejected the pattern code: {error}")),
    }
}

unsafe extern "C" fn sre_getcodesize(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    alloc_int_object(vm::CODESIZE as i64)
}

unsafe extern "C" fn sre_template(_argv: *mut *mut PyObject, _argc: usize) -> *mut PyObject {
    fail("_sre.template is not implemented; pon expands sub() templates natively")
}

unsafe fn char_argument(argv: *mut *mut PyObject, argc: usize, name: &str) -> Option<i64> {
    let args = unsafe { arg_slice(argv, argc) }?;
    if args.len() != 1 {
        pon_err_set(format!("_sre.{name}() expected 1 argument, got {}", args.len()));
        return None;
    }
    let value = to_i64(args[0]);
    if value.is_none() {
        pon_err_set(format!("_sre.{name}() argument must be an int"));
    }
    value
}

unsafe extern "C" fn sre_ascii_iscased(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(ch) = (unsafe { char_argument(argv, argc, "ascii_iscased") }) else {
        return ptr::null_mut();
    };
    bool_::from_bool((65..=90).contains(&ch) || (97..=122).contains(&ch))
}

unsafe extern "C" fn sre_unicode_iscased(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(ch) = (unsafe { char_argument(argv, argc, "unicode_iscased") }) else {
        return ptr::null_mut();
    };
    let cased = u32::try_from(ch)
        .ok()
        .and_then(char::from_u32)
        .is_some_and(|c| c.is_lowercase() || c.is_uppercase());
    bool_::from_bool(cased)
}

unsafe extern "C" fn sre_ascii_tolower(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(ch) = (unsafe { char_argument(argv, argc, "ascii_tolower") }) else {
        return ptr::null_mut();
    };
    alloc_int_object(if (65..=90).contains(&ch) { ch + 32 } else { ch })
}

unsafe extern "C" fn sre_unicode_tolower(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(ch) = (unsafe { char_argument(argv, argc, "unicode_tolower") }) else {
        return ptr::null_mut();
    };
    let lowered = u32::try_from(ch).ok().and_then(char::from_u32).map_or(ch, |c| {
        let mut lower = c.to_lowercase();
        match (lower.next(), lower.next()) {
            (Some(single), None) => i64::from(u32::from(single)),
            _ => ch,
        }
    });
    alloc_int_object(lowered)
}

// ---------------------------------------------------------------------------
// Pattern object

unsafe extern "C" fn pattern_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name_text) = (unsafe { unicode_text(untag(name)) }) else {
        return fail("attribute name must be str");
    };
    let Some(pattern) = (unsafe { as_pattern(object) }) else {
        return fail("re.Pattern receiver is invalid");
    };
    match name_text {
        "pattern" => pattern.pattern_obj,
        "flags" => alloc_int_object(pattern.flags),
        "groups" => alloc_int_object(pattern.pattern.groups() as i64),
        "groupindex" => pattern.groupindex_obj,
        "match" => bound_method(object, name_text, pattern_match_method),
        "fullmatch" => bound_method(object, name_text, pattern_fullmatch_method),
        "search" => bound_method(object, name_text, pattern_search_method),
        "findall" => bound_method(object, name_text, pattern_findall_method),
        "finditer" => bound_method(object, name_text, pattern_finditer_method),
        "split" => bound_method(object, name_text, pattern_split_method),
        "sub" => bound_method(object, name_text, pattern_sub_method),
        "subn" => bound_method(object, name_text, pattern_subn_method),
        // SAFETY: Raise helper with the interned attribute name.
        _ => unsafe { abi::pon_raise_attribute_error(object, intern(name_text)) },
    }
}

/// CPython `pattern_repr` flag table (Modules/_sre/sre.c order).
const FLAG_NAMES: [(&str, i64); 8] = [
    ("re.IGNORECASE", 2),
    ("re.LOCALE", 4),
    ("re.MULTILINE", 8),
    ("re.DOTALL", 16),
    ("re.UNICODE", 32),
    ("re.VERBOSE", 64),
    ("re.DEBUG", 128),
    ("re.ASCII", 256),
];

/// Renders `flags` the way CPython's `Pattern.__repr__` does: named flags in
/// table order joined by `|`, leftover bits as a trailing hex literal, and the
/// implicit `re.UNICODE` omitted for str patterns.
fn pattern_flags_repr(mut flags: i64, pattern_is_str: bool) -> Option<String> {
    const LOCALE: i64 = 4;
    const UNICODE: i64 = 32;
    const ASCII: i64 = 256;
    if pattern_is_str && flags & (LOCALE | UNICODE | ASCII) == UNICODE {
        flags &= !UNICODE;
    }
    if flags == 0 {
        return None;
    }
    let mut parts = Vec::new();
    for (name, value) in FLAG_NAMES {
        if flags & value != 0 {
            parts.push((*name).to_owned());
            flags &= !value;
        }
    }
    if flags != 0 {
        parts.push(format!("{flags:#x}"));
    }
    Some(parts.join("|"))
}

unsafe extern "C" fn pattern_repr(object: *mut PyObject) -> *mut PyObject {
    let Some(pattern) = (unsafe { as_pattern(object) }) else {
        return fail("re.Pattern receiver is invalid");
    };
    // SAFETY: `pattern_obj` is heap-or-NULL; `unicode_text` rejects NULL.
    let is_str = unsafe { unicode_text(pattern.pattern_obj) }.is_some();
    let flags_suffix = pattern_flags_repr(pattern.flags, is_str)
        .map_or_else(String::new, |text| format!(", {text}"));
    alloc_str_object(&format!(
        "re.compile({}{flags_suffix})",
        clipped_repr(pattern.pattern_obj, 200)
    ))
}

fn bound_method(receiver: *mut PyObject, name: &str, entry: BuiltinFn) -> *mut PyObject {
    // SAFETY: `entry` is a live builtin entry point with the runtime calling convention.
    let function = unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
    if function.is_null() {
        return ptr::null_mut();
    }
    match crate::types::method::new_bound_method(function, receiver) {
        Ok(method) => method.cast::<PyObject>(),
        Err(message) => fail(message),
    }
}

#[derive(Clone, Copy)]
enum MatchMode {
    Match,
    Fullmatch,
    Search,
}

/// Run `mode` matching honoring CPython `pos`/`endpos`: matching starts at
/// `pos` (so `^`/`\A` still anchor at the real beginning, not `pos`) and the
/// subject is treated as `endpos` units long, so `$`/`\Z` and character
/// availability stop there.  Both bounds are pre-clamped to `[0, units]` with
/// `pos <= endpos`.
fn run_match(
    pattern: &vm::Pattern,
    subject: &Subject,
    mode: MatchMode,
    pos: usize,
    endpos: usize,
) -> Result<Option<vm::Match>, vm::Error> {
    match subject {
        Subject::Str { text, offsets } => {
            let sub = &text[..offsets[endpos]];
            match mode {
                MatchMode::Match => pattern.match_str_at(sub, pos),
                MatchMode::Fullmatch => pattern.fullmatch_str_at(sub, pos),
                MatchMode::Search => pattern.search_str_at(sub, pos),
            }
        }
        Subject::Bytes { data } => {
            let sub = &data[..endpos];
            match mode {
                MatchMode::Match => pattern.match_bytes_at(sub, pos),
                MatchMode::Fullmatch => pattern.fullmatch_bytes_at(sub, pos),
                MatchMode::Search => pattern.search_bytes_at(sub, pos),
            }
        }
    }
}

fn run_finditer(pattern: &vm::Pattern, subject: &Subject) -> Result<Vec<vm::Match>, vm::Error> {
    match subject {
        Subject::Str { text, .. } => pattern.finditer_str(text),
        Subject::Bytes { data } => pattern.finditer_bytes(data),
    }
}

/// Extracts `(receiver, subject, extra args)` shared by every Pattern method.
unsafe fn pattern_method_prelude<'a>(
    argv: *mut *mut PyObject,
    argc: usize,
    name: &str,
    subject_index: usize,
) -> Option<(&'a mut SrePattern, Subject, &'a [*mut PyObject])> {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        pon_err_set(format!("{name}() received a null argv pointer"));
        return None;
    };
    if args.len() <= subject_index {
        pon_err_set(format!("{name}() missing required argument"));
        return None;
    }
    let Some(pattern) = (unsafe { as_pattern(args[0]) }) else {
        pon_err_set(format!("{name}() receiver is not an re.Pattern"));
        return None;
    };
    let Some(subject) = Subject::from_object(args[subject_index]) else {
        pon_err_set(format!("{name}() expected a str or bytes subject"));
        return None;
    };
    Some((pattern, subject, args))
}

/// Read an optional integer position argument (`pos`/`endpos`), clamped to
/// `[0, units]`.  A missing argument yields `default`; a non-integer sets a
/// `TypeError` and returns `Err`.
fn optional_bound(args: &[*mut PyObject], index: usize, default: usize, units: usize, name: &str) -> Result<usize, ()> {
    let Some(&arg) = args.get(index) else {
        return Ok(default);
    };
    match to_i64(arg) {
        Some(value) => Ok(value.clamp(0, units as i64) as usize),
        None => {
            pon_err_set(format!("{name}() expected an integer position argument"));
            Err(())
        }
    }
}

unsafe fn pattern_run_match(argv: *mut *mut PyObject, argc: usize, name: &str, mode: MatchMode) -> *mut PyObject {
    let Some((pattern, subject, args)) = (unsafe { pattern_method_prelude(argv, argc, name, 1) }) else {
        return ptr::null_mut();
    };
    let units = subject.units();
    let (Ok(pos), Ok(endpos)) = (
        optional_bound(args, 2, 0, units, name),
        optional_bound(args, 3, units, units, name),
    ) else {
        return ptr::null_mut();
    };
    if pos > endpos {
        return none();
    }
    match run_match(&pattern.pattern, &subject, mode, pos, endpos) {
        Ok(Some(matched)) => alloc_match(matched, args[0], untag(args[1])),
        Ok(None) => none(),
        Err(error) => fail(format!("{name}(): {error}")),
    }
}

unsafe extern "C" fn pattern_match_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { pattern_run_match(argv, argc, "match", MatchMode::Match) }
}

unsafe extern "C" fn pattern_fullmatch_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { pattern_run_match(argv, argc, "fullmatch", MatchMode::Fullmatch) }
}

unsafe extern "C" fn pattern_search_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { pattern_run_match(argv, argc, "search", MatchMode::Search) }
}

unsafe extern "C" fn pattern_findall_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some((pattern, subject, _args)) = (unsafe { pattern_method_prelude(argv, argc, "findall", 1) }) else {
        return ptr::null_mut();
    };
    let rows = match &subject {
        Subject::Str { text, .. } => pattern.pattern.findall_str(text),
        Subject::Bytes { data } => pattern.pattern.findall_bytes(data),
    };
    let rows = match rows {
        Ok(rows) => rows,
        Err(error) => return fail(format!("findall(): {error}")),
    };
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let mut columns = Vec::with_capacity(row.len());
        for value in &row {
            // CPython findall maps unmatched groups to the empty string/bytes.
            columns.push(match value {
                Some(value) => matched_value_object(value),
                None => subject.make_object(&[]),
            });
        }
        if columns.len() == 1 {
            items.push(columns.pop().expect("one column"));
        } else {
            items.push(alloc_tuple(columns));
        }
    }
    alloc_list(items)
}

unsafe extern "C" fn pattern_finditer_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some((pattern, subject, args)) = (unsafe { pattern_method_prelude(argv, argc, "finditer", 1) }) else {
        return ptr::null_mut();
    };
    match run_finditer(&pattern.pattern, &subject) {
        Ok(matches) => {
            let string_obj = untag(args[1]);
            let items = matches
                .into_iter()
                .map(|matched| alloc_match(matched, args[0], string_obj))
                .collect();
            alloc_iterator(items)
        }
        Err(error) => fail(format!("finditer(): {error}")),
    }
}

unsafe extern "C" fn pattern_split_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some((pattern, subject, args)) = (unsafe { pattern_method_prelude(argv, argc, "split", 1) }) else {
        return ptr::null_mut();
    };
    let maxsplit = args.get(2).copied().and_then(to_i64).unwrap_or(0);
    let matches = match run_finditer(&pattern.pattern, &subject) {
        Ok(matches) => matches,
        Err(error) => return fail(format!("split(): {error}")),
    };
    let groups = pattern.pattern.groups();
    let mut items = Vec::new();
    let mut last = 0usize;
    let mut splits = 0i64;
    for matched in &matches {
        if maxsplit > 0 && splits >= maxsplit {
            break;
        }
        let Some(Some((start, end))) = matched.span(0) else { continue };
        items.push(subject.make_object(subject.slice_bytes(last, start)));
        for group in 1..=groups {
            // CPython split emits None for groups that did not participate.
            match matched.group(group) {
                Some(value) => items.push(matched_value_object(&value)),
                None => items.push(none()),
            }
        }
        last = end;
        splits += 1;
    }
    items.push(subject.make_object(subject.slice_bytes(last, subject.units())));
    alloc_list(items)
}

unsafe extern "C" fn pattern_sub_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { pattern_run_sub(argv, argc, "sub", false) }
}

unsafe extern "C" fn pattern_subn_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    unsafe { pattern_run_sub(argv, argc, "subn", true) }
}

enum Repl {
    Template(Vec<ReplPiece>),
    Callable(*mut PyObject),
}

unsafe fn pattern_run_sub(argv: *mut *mut PyObject, argc: usize, name: &str, with_count: bool) -> *mut PyObject {
    let Some((pattern, subject, args)) = (unsafe { pattern_method_prelude(argv, argc, name, 2) }) else {
        return ptr::null_mut();
    };
    let count = args.get(3).copied().and_then(to_i64).unwrap_or(0);
    let repl_obj = untag(args[1]);
    // SAFETY: heap-or-NULL after untagging; accessors reject NULL.
    let repl = if let Some(text) = unsafe { unicode_text(repl_obj) } {
        if subject.is_bytes() {
            return fail(format!("{name}(): cannot use a str replacement on a bytes pattern"));
        }
        match parse_template(text.as_bytes(), false) {
            Ok(pieces) => Repl::Template(pieces),
            Err(message) => return fail(format!("{name}(): {message}")),
        }
    } else if !repl_obj.is_null() && bytes_::is_bytes_type(unsafe { (*repl_obj).ob_type }) {
        if !subject.is_bytes() {
            return fail(format!("{name}(): cannot use a bytes replacement on a str pattern"));
        }
        // SAFETY: The type check above guarantees a live `PyBytes`.
        let data = unsafe { (*repl_obj.cast::<bytes_::PyBytes>()).as_slice() }.to_vec();
        match parse_template(&data, true) {
            Ok(pieces) => Repl::Template(pieces),
            Err(message) => return fail(format!("{name}(): {message}")),
        }
    } else {
        Repl::Callable(repl_obj)
    };

    let matches = match run_finditer(&pattern.pattern, &subject) {
        Ok(matches) => matches,
        Err(error) => return fail(format!("{name}(): {error}")),
    };

    let mut out: Vec<u8> = Vec::new();
    let mut last = 0usize;
    let mut replaced = 0i64;
    for matched in &matches {
        if count > 0 && replaced >= count {
            break;
        }
        let Some(Some((start, end))) = matched.span(0) else { continue };
        out.extend_from_slice(subject.slice_bytes(last, start));
        match &repl {
            Repl::Template(pieces) => match expand_template(pieces, matched) {
                Ok(bytes) => out.extend_from_slice(&bytes),
                Err(message) => return fail(format!("{name}(): {message}")),
            },
            Repl::Callable(callable) => {
                let match_obj = alloc_match(matched.clone(), args[0], untag(args[2]));
                let mut call_args = [match_obj];
                // SAFETY: `pon_call` self-normalizes; argv points at one live slot.
                let result = unsafe { abi::pon_call(*callable, call_args.as_mut_ptr(), call_args.len()) };
                if result.is_null() {
                    return ptr::null_mut();
                }
                let result = untag(result);
                // SAFETY: heap-or-NULL after untagging; accessors reject NULL.
                if let Some(text) = unsafe { unicode_text(result) } {
                    if subject.is_bytes() {
                        return fail(format!("{name}(): replacement returned str for a bytes pattern"));
                    }
                    out.extend_from_slice(text.as_bytes());
                } else if !result.is_null() && bytes_::is_bytes_type(unsafe { (*result).ob_type }) {
                    if !subject.is_bytes() {
                        return fail(format!("{name}(): replacement returned bytes for a str pattern"));
                    }
                    // SAFETY: The type check above guarantees a live `PyBytes`.
                    out.extend_from_slice(unsafe { (*result.cast::<bytes_::PyBytes>()).as_slice() });
                } else {
                    return fail(format!("{name}(): replacement callable must return str or bytes"));
                }
            }
        }
        last = end;
        replaced += 1;
    }
    out.extend_from_slice(subject.slice_bytes(last, subject.units()));
    let result = subject.make_object(&out);
    if with_count {
        alloc_tuple(vec![result, alloc_int_object(replaced)])
    } else {
        result
    }
}

// ---------------------------------------------------------------------------
// Replacement templates (CPython `re._parser.parse_template` subset)

enum ReplPiece {
    Literal(Vec<u8>),
    Group(GroupSelector),
}

enum GroupSelector {
    Index(usize),
    Name(String),
}

fn parse_template(repl: &[u8], is_bytes: bool) -> Result<Vec<ReplPiece>, String> {
    fn push_char(literal: &mut Vec<u8>, value: u32, is_bytes: bool) {
        if is_bytes || value < 0x80 {
            literal.push(value as u8);
        } else if let Some(c) = char::from_u32(value) {
            let mut buffer = [0u8; 4];
            literal.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
        }
    }

    let mut pieces = Vec::new();
    let mut literal: Vec<u8> = Vec::new();
    let mut index = 0usize;
    while index < repl.len() {
        let byte = repl[index];
        if byte != b'\\' {
            literal.push(byte);
            index += 1;
            continue;
        }
        index += 1;
        let Some(&escape) = repl.get(index) else {
            return Err("bad escape (end of pattern)".to_owned());
        };
        index += 1;
        match escape {
            b'\\' => literal.push(b'\\'),
            b'n' => literal.push(b'\n'),
            b'r' => literal.push(b'\r'),
            b't' => literal.push(b'\t'),
            b'v' => literal.push(0x0b),
            b'f' => literal.push(0x0c),
            b'a' => literal.push(0x07),
            b'b' => literal.push(0x08),
            b'g' => {
                if repl.get(index) != Some(&b'<') {
                    return Err("missing <".to_owned());
                }
                index += 1;
                let start = index;
                while index < repl.len() && repl[index] != b'>' {
                    index += 1;
                }
                if index >= repl.len() {
                    return Err("missing >, unterminated name".to_owned());
                }
                let raw_name = &repl[start..index];
                index += 1;
                if raw_name.is_empty() {
                    return Err("missing group name".to_owned());
                }
                if !literal.is_empty() {
                    pieces.push(ReplPiece::Literal(std::mem::take(&mut literal)));
                }
                if raw_name.iter().all(u8::is_ascii_digit) {
                    let group = std::str::from_utf8(raw_name)
                        .ok()
                        .and_then(|text| text.parse::<usize>().ok())
                        .ok_or_else(|| "invalid group reference".to_owned())?;
                    pieces.push(ReplPiece::Group(GroupSelector::Index(group)));
                } else {
                    let name = String::from_utf8(raw_name.to_vec())
                        .map_err(|_| "bad character in group name".to_owned())?;
                    pieces.push(ReplPiece::Group(GroupSelector::Name(name)));
                }
            }
            b'0' => {
                // Octal escape: `\0` plus up to two more octal digits.
                let mut value = 0u32;
                let mut consumed = 0;
                while consumed < 2 {
                    match repl.get(index) {
                        Some(&digit) if (b'0'..=b'7').contains(&digit) => {
                            value = value * 8 + u32::from(digit - b'0');
                            index += 1;
                            consumed += 1;
                        }
                        _ => break,
                    }
                }
                push_char(&mut literal, value, is_bytes);
            }
            b'1'..=b'9' => {
                let mut digits = vec![escape];
                if repl.get(index).is_some_and(u8::is_ascii_digit) {
                    digits.push(repl[index]);
                    index += 1;
                    // Three consecutive octal digits form an octal escape.
                    let all_octal = digits.iter().all(|digit| (b'0'..=b'7').contains(digit));
                    if all_octal && repl.get(index).is_some_and(|digit| (b'0'..=b'7').contains(digit)) {
                        digits.push(repl[index]);
                        index += 1;
                        let value = digits.iter().fold(0u32, |acc, digit| acc * 8 + u32::from(digit - b'0'));
                        if value > 0o377 {
                            return Err(format!(
                                "octal escape value \\{} outside of range 0-0o377",
                                String::from_utf8_lossy(&digits)
                            ));
                        }
                        push_char(&mut literal, value, is_bytes);
                        continue;
                    }
                }
                let group = std::str::from_utf8(&digits)
                    .ok()
                    .and_then(|text| text.parse::<usize>().ok())
                    .ok_or_else(|| "invalid group reference".to_owned())?;
                if !literal.is_empty() {
                    pieces.push(ReplPiece::Literal(std::mem::take(&mut literal)));
                }
                pieces.push(ReplPiece::Group(GroupSelector::Index(group)));
            }
            other if other.is_ascii_alphabetic() => {
                return Err(format!("bad escape \\{}", char::from(other)));
            }
            other => {
                literal.push(b'\\');
                literal.push(other);
            }
        }
    }
    if !literal.is_empty() {
        pieces.push(ReplPiece::Literal(literal));
    }
    Ok(pieces)
}

fn expand_template(pieces: &[ReplPiece], matched: &vm::Match) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for piece in pieces {
        match piece {
            ReplPiece::Literal(bytes) => out.extend_from_slice(bytes),
            ReplPiece::Group(selector) => {
                let index = match selector {
                    GroupSelector::Index(index) => *index,
                    GroupSelector::Name(name) => *matched
                        .groupindex()
                        .get(name)
                        .ok_or_else(|| format!("unknown group name {name:?}"))?,
                };
                match matched.span(index) {
                    None => return Err(format!("invalid group reference {index}")),
                    // Unmatched groups expand to the empty string (CPython 3.5+).
                    Some(None) => {}
                    Some(Some(_)) => {
                        if let Some(value) = matched.group(index) {
                            match value {
                                vm::MatchedValue::Str(text) => out.extend_from_slice(text.as_bytes()),
                                vm::MatchedValue::Bytes(bytes) => out.extend_from_slice(&bytes),
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Match object

unsafe extern "C" fn match_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name_text) = (unsafe { unicode_text(untag(name)) }) else {
        return fail("attribute name must be str");
    };
    let Some(matched) = (unsafe { as_match(object) }) else {
        return fail("re.Match receiver is invalid");
    };
    match name_text {
        "re" => matched.pattern_obj,
        "string" => matched.string_obj,
        "lastindex" => matched
            .matched
            .lastindex()
            .map_or_else(none, |index| alloc_int_object(index as i64)),
        "lastgroup" => matched.matched.lastgroup().map_or_else(none, alloc_str_object),
        "group" => bound_method(object, name_text, match_group_method),
        "groups" => bound_method(object, name_text, match_groups_method),
        "groupdict" => bound_method(object, name_text, match_groupdict_method),
        "span" => bound_method(object, name_text, match_span_method),
        "start" => bound_method(object, name_text, match_start_method),
        "end" => bound_method(object, name_text, match_end_method),
        // SAFETY: Raise helper with the interned attribute name.
        _ => unsafe { abi::pon_raise_attribute_error(object, intern(name_text)) },
    }
}

unsafe extern "C" fn match_repr(object: *mut PyObject) -> *mut PyObject {
    let Some(matched) = (unsafe { as_match(object) }) else {
        return fail("re.Match receiver is invalid");
    };
    let (start, end) = matched.matched.span(0).flatten().unwrap_or((0, 0));
    let group0 = matched
        .matched
        .group(0)
        .map_or_else(|| none(), |value| matched_value_object(&value));
    alloc_str_object(&format!(
        "<re.Match object; span=({start}, {end}), match={}>",
        clipped_repr(group0, 50)
    ))
}

fn resolve_group_selector(matched: &vm::Match, selector: *mut PyObject) -> Result<usize, String> {
    let selector = untag(selector);
    // SAFETY: heap-or-NULL after untagging; accessors reject NULL.
    if let Some(name) = unsafe { unicode_text(selector) } {
        return matched.groupindex().get(name).copied().ok_or_else(|| "no such group".to_owned());
    }
    if let Some(index) = to_i64(selector) {
        if index >= 0 && matched.span(index as usize).is_some() {
            return Ok(index as usize);
        }
    }
    Err("no such group".to_owned())
}

fn group_value(matched: &vm::Match, index: usize) -> *mut PyObject {
    match matched.span(index) {
        None => fail("no such group"),
        Some(None) => none(),
        Some(Some(_)) => matched
            .group(index)
            .map_or_else(none, |value| matched_value_object(&value)),
    }
}

/// `re.Match` subscript slot: `m[group]` delegates to `m.group(group)` for a
/// single int or str selector (CPython `match_getitem`), returning the group's
/// text, `None` for an unmatched optional group, or an error for a bad group.
unsafe extern "C" fn match_subscript(object: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
    let Some(matched) = (unsafe { as_match(object) }) else {
        return fail("expected an re.Match object");
    };
    match resolve_group_selector(&matched.matched, key) {
        Ok(index) => group_value(&matched.matched, index),
        Err(message) => fail(message),
    }
}

/// Extracts the Match receiver plus the trailing arguments of a bound call.
unsafe fn match_method_prelude<'a>(
    argv: *mut *mut PyObject,
    argc: usize,
    name: &str,
) -> Option<(&'a mut SreMatch, &'a [*mut PyObject])> {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        pon_err_set(format!("{name}() received a null argv pointer"));
        return None;
    };
    let Some((&receiver, rest)) = args.split_first() else {
        pon_err_set(format!("{name}() missing receiver"));
        return None;
    };
    let Some(matched) = (unsafe { as_match(receiver) }) else {
        pon_err_set(format!("{name}() receiver is not an re.Match"));
        return None;
    };
    Some((matched, rest))
}

unsafe extern "C" fn match_group_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some((matched, selectors)) = (unsafe { match_method_prelude(argv, argc, "group") }) else {
        return ptr::null_mut();
    };
    if selectors.is_empty() {
        return group_value(&matched.matched, 0);
    }
    let mut values = Vec::with_capacity(selectors.len());
    for &selector in selectors {
        let index = match resolve_group_selector(&matched.matched, selector) {
            Ok(index) => index,
            Err(message) => return fail(message),
        };
        let value = group_value(&matched.matched, index);
        if value.is_null() {
            return ptr::null_mut();
        }
        values.push(value);
    }
    if values.len() == 1 {
        values.pop().expect("one value")
    } else {
        alloc_tuple(values)
    }
}

unsafe extern "C" fn match_groups_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some((matched, rest)) = (unsafe { match_method_prelude(argv, argc, "groups") }) else {
        return ptr::null_mut();
    };
    let default = rest.first().map_or_else(none, |&value| untag(value));
    let values = matched
        .matched
        .groups()
        .into_iter()
        .map(|value| value.map_or(default, |value| matched_value_object(&value)))
        .collect();
    alloc_tuple(values)
}

unsafe extern "C" fn match_groupdict_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some((matched, rest)) = (unsafe { match_method_prelude(argv, argc, "groupdict") }) else {
        return ptr::null_mut();
    };
    let default = rest.first().map_or_else(none, |&value| untag(value));
    // Emit names in group-definition order (index order), like CPython's dict.
    let mut names: Vec<(&String, usize)> = matched
        .matched
        .groupindex()
        .iter()
        .map(|(name, index)| (name, *index))
        .collect();
    names.sort_by_key(|(_, index)| *index);
    let mut flat = Vec::with_capacity(names.len() * 2);
    for (name, index) in names {
        flat.push(alloc_str_object(name));
        let value = match matched.matched.span(index) {
            Some(Some(_)) => matched
                .matched
                .group(index)
                .map_or(default, |value| matched_value_object(&value)),
            _ => default,
        };
        flat.push(value);
    }
    let pair_count = flat.len() / 2;
    // SAFETY: `flat` holds `pair_count` live key/value pairs.
    unsafe { abi::map::pon_build_map(flat.as_mut_ptr(), pair_count) }
}

unsafe fn match_span_index(argv: *mut *mut PyObject, argc: usize, name: &str) -> Option<(&'static mut SreMatch, usize)> {
    let (matched, rest) = unsafe { match_method_prelude(argv, argc, name) }?;
    let index = match rest.first() {
        None => 0,
        Some(&selector) => match resolve_group_selector(&matched.matched, selector) {
            Ok(index) => index,
            Err(message) => {
                pon_err_set(message);
                return None;
            }
        },
    };
    if matched.matched.span(index).is_none() {
        pon_err_set("no such group");
        return None;
    }
    Some((matched, index))
}

unsafe extern "C" fn match_span_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some((matched, index)) = (unsafe { match_span_index(argv, argc, "span") }) else {
        return ptr::null_mut();
    };
    let (start, end) = match matched.matched.span(index) {
        Some(Some((start, end))) => (start as i64, end as i64),
        _ => (-1, -1),
    };
    alloc_tuple(vec![alloc_int_object(start), alloc_int_object(end)])
}

unsafe extern "C" fn match_start_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some((matched, index)) = (unsafe { match_span_index(argv, argc, "start") }) else {
        return ptr::null_mut();
    };
    alloc_int_object(matched.matched.start(index).map_or(-1, |start| start as i64))
}

unsafe extern "C" fn match_end_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some((matched, index)) = (unsafe { match_span_index(argv, argc, "end") }) else {
        return ptr::null_mut();
    };
    alloc_int_object(matched.matched.end(index).map_or(-1, |end| end as i64))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::native::builtins_mod::str_text;
    use crate::thread_state::test_state_lock;

    /// `_sre` code units for `re.compile('a+')` (fixtures.json
    /// `sre_curated_019`, CPython 3.14 encoding): INFO block followed by
    /// `REPEAT_ONE LITERAL 'a' SUCCESS`.
    const A_PLUS_CODE: [u32; 13] = [14, 4, 0, 1, 4_294_967_295, 24, 6, 1, 4_294_967_295, 16, 97, 1, 1];
    /// `re.UNICODE`, the compiler-implied flag for str patterns.
    const FLAG_UNICODE: i64 = 32;

    fn init_runtime() {
        assert_eq!(unsafe { abi::pon_runtime_init() }, 0);
        pon_err_clear();
    }

    fn compiled_a_plus() -> vm::Pattern {
        vm::compile(
            vm::PatternText::Str("a+".to_owned()),
            FLAG_UNICODE as u32,
            A_PLUS_CODE.to_vec(),
            0,
            BTreeMap::new(),
            vec![None],
        )
        .expect("a+ opcode vector compiles")
    }

    fn pattern_object(pattern_text: &str) -> *mut PyObject {
        alloc_pattern(compiled_a_plus(), alloc_str_object(pattern_text), none(), FLAG_UNICODE)
    }

    #[test]
    fn pattern_and_match_repr_dispatch_matches_cpython() {
        let _guard = test_state_lock();
        init_runtime();
        let pattern_obj = pattern_object("a+");
        // repr()/str() route through tp_repr/tp_str via the dispatch layer;
        // neither type name is in the native repr whitelist.
        assert_eq!(repr_text(pattern_obj), "re.compile('a+')");
        assert_eq!(str_text(pattern_obj), "re.compile('a+')");

        let matched = compiled_a_plus()
            .match_str("aaab")
            .expect("a+ executes")
            .expect("a+ matches aaab");
        let match_obj = alloc_match(matched, pattern_obj, alloc_str_object("aaab"));
        assert_eq!(repr_text(match_obj), "<re.Match object; span=(0, 3), match='aaa'>");
        assert_eq!(str_text(match_obj), "<re.Match object; span=(0, 3), match='aaa'>");
    }

    #[test]
    fn pattern_and_match_reprs_clip_like_cpython() {
        let _guard = test_state_lock();
        init_runtime();
        // CPython `%.200R`: `repr(re.compile('a' * 210))` keeps the first 200
        // code points of the pattern repr (opening quote plus 199 chars, no
        // closing quote).
        let long_pattern = "a".repeat(210);
        let pattern_obj = pattern_object(&long_pattern);
        let expected = format!("re.compile('{})", "a".repeat(199));
        assert_eq!(repr_text(pattern_obj), expected);

        // CPython `%.50R`: the matched text repr is clipped to 50 code points.
        let subject = "a".repeat(80);
        let matched = compiled_a_plus()
            .match_str(&subject)
            .expect("a+ executes")
            .expect("a+ matches the long subject");
        let match_obj = alloc_match(matched, pattern_obj, alloc_str_object(&subject));
        let expected = format!("<re.Match object; span=(0, 80), match='{}>", "a".repeat(49));
        assert_eq!(repr_text(match_obj), expected);
    }
}
