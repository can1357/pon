//! Native `_lsprof` compatibility surface.
//!
//! Profiling data collection requires frame-tracing hooks Pon does not have.
//! The `Profiler` type is real and stateful (`enable`, `disable`, and `clear`
//! update profiler state), but the recorded sample set is intentionally empty:
//! `getstats()` returns `[]` instead of fabricating call records.

use std::collections::HashMap;
use std::ptr;
use std::sync::{LazyLock, Mutex};

use crate::abi;
use crate::intern::intern;
use crate::object::{PyObject, PyType};
use crate::types::exc::ExceptionKind;

use super::builtins_mod::VARIADIC_ARITY;
use super::install_module;

#[derive(Clone, Copy, Debug)]
struct ProfilerState {
    enabled: bool,
    subcalls: bool,
    builtins: bool,
    clear_count: u64,
}

impl Default for ProfilerState {
    fn default() -> Self {
        Self {
            enabled: false,
            subcalls: true,
            builtins: true,
            clear_count: 0,
        }
    }
}

static STATES: LazyLock<Mutex<HashMap<usize, ProfilerState>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let name = "_lsprof";
    let name_object = str_object(name).ok_or_else(|| "failed to allocate _lsprof.__name__".to_owned())?;
    let attrs = vec![(intern("__name__"), name_object), (intern("Profiler"), profiler_type().cast::<PyObject>())];
    install_module(name, attrs)
}

static PROFILER_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(
        abi::runtime_type_type().cast_const(),
        "_lsprof.Profiler",
        core::mem::size_of::<crate::types::type_::PyHeapInstance>(),
    );
    ty.tp_base = abi::runtime_global(intern("object")).map_or(ptr::null_mut(), |object| object.cast::<PyType>());
    ty.tp_dictoffset = 1;
    ty.tp_getattro = Some(profiler_getattro);
    ty.tp_setattro = Some(crate::descr::generic_set_attr);
    ty.tp_new = Some(crate::types::type_::type_new);
    ty.tp_init = Some(crate::types::type_::type_init);
    ty.gc_type_id = crate::types::type_::TYPE_ID_HEAP_INSTANCE.0 as usize;

    let namespace = crate::types::type_::new_namespace();
    set_str(namespace, "__doc__", "Profiler(timer=None, timeunit=None, subcalls=True, builtins=True)");
    set_str(namespace, "__module__", "_lsprof");
    for &(method_name, entry) in PROFILER_METHODS {
        set_function(namespace, method_name, entry);
    }
    ty.tp_dict = namespace.cast::<PyObject>();

    let ty = Box::into_raw(Box::new(ty));
    crate::sync::register_namespaced_type(ty);
    crate::sync::type_modified(ty);
    ty as usize
});

fn profiler_type() -> *mut PyType {
    *PROFILER_TYPE as *mut PyType
}

const PROFILER_METHODS: &[(&str, unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject)] = &[
    ("__init__", profiler_init),
    ("clear", profiler_clear),
    ("disable", profiler_disable),
    ("enable", profiler_enable),
    ("getstats", profiler_getstats),
];

fn set_str(namespace: *mut crate::types::type_::PyClassDict, name: &str, value: &str) {
    if let Some(object) = str_object(value) {
        unsafe { (&mut *namespace).set(intern(name), object) };
    }
}

fn str_object(value: &str) -> Option<*mut PyObject> {
    // SAFETY: Runtime allocation helper returns NULL with a diagnostic on failure.
    let object = unsafe { abi::pon_const_str(value.as_ptr(), value.len()) };
    (!object.is_null()).then_some(object)
}

fn set_function(
    namespace: *mut crate::types::type_::PyClassDict,
    name: &str,
    entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) {
    let interned = intern(name);
    // SAFETY: Live native entry point with the runtime calling convention.
    let function = unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, interned) };
    if !function.is_null() {
        unsafe { (&mut *namespace).set(interned, function) };
    }
}

unsafe extern "C" fn profiler_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name_text) = (unsafe { crate::types::type_::unicode_text(crate::tag::untag_arg(name)) }) else {
        return raise(ExceptionKind::TypeError, "attribute name must be str");
    };
    match name_text {
        "clear" => bound_method(object, name_text, profiler_clear),
        "disable" => bound_method(object, name_text, profiler_disable),
        "enable" => bound_method(object, name_text, profiler_enable),
        "getstats" => bound_method(object, name_text, profiler_getstats),
        _ => unsafe { crate::descr::generic_get_attr(object, name) },
    }
}

fn bound_method(
    receiver: *mut PyObject,
    name: &str,
    entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> *mut PyObject {
    // SAFETY: Live native entry point with the runtime calling convention.
    let function = unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
    if function.is_null() {
        return ptr::null_mut();
    }
    match crate::types::method::new_bound_method(function, receiver) {
        Ok(method) => method.cast::<PyObject>(),
        Err(message) => crate::abi::exc::raise_kind_error_text(ExceptionKind::RuntimeError, &message),
    }
}

unsafe extern "C" fn profiler_init(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (receiver, args) = match unsafe { method_args(argv, argc, "Profiler.__init__") } {
        Ok(parts) => parts,
        Err(error) => return error,
    };
    if args.len() > 4 {
        return raise(
            ExceptionKind::TypeError,
            &format!("Profiler expected at most 4 arguments, got {}", args.len()),
        );
    }

    let mut state = ProfilerState::default();
    if let Some(&subcalls) = args.get(2) {
        state.subcalls = match bool_arg(subcalls) {
            Some(value) => value,
            None => return ptr::null_mut(),
        };
    }
    if let Some(&builtins) = args.get(3) {
        state.builtins = match bool_arg(builtins) {
            Some(value) => value,
            None => return ptr::null_mut(),
        };
    }

    let mut states = STATES.lock().unwrap_or_else(|poison| poison.into_inner());
    states.insert(receiver as usize, state);
    none()
}

unsafe extern "C" fn profiler_enable(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (receiver, args) = match unsafe { method_args(argv, argc, "enable") } {
        Ok(parts) => parts,
        Err(error) => return error,
    };
    if args.len() > 2 {
        return raise(ExceptionKind::TypeError, &format!("enable expected at most 2 arguments, got {}", args.len()));
    }
    let subcalls = match optional_bool(args.first().copied(), true) {
        Some(value) => value,
        None => return ptr::null_mut(),
    };
    let builtins = match optional_bool(args.get(1).copied(), true) {
        Some(value) => value,
        None => return ptr::null_mut(),
    };

    let mut states = STATES.lock().unwrap_or_else(|poison| poison.into_inner());
    let state = states.entry(receiver as usize).or_default();
    state.enabled = true;
    state.subcalls = subcalls;
    state.builtins = builtins;
    none()
}

unsafe extern "C" fn profiler_disable(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (receiver, args) = match unsafe { method_args(argv, argc, "disable") } {
        Ok(parts) => parts,
        Err(error) => return error,
    };
    if !args.is_empty() {
        return raise(ExceptionKind::TypeError, &format!("disable expected no arguments, got {}", args.len()));
    }
    let mut states = STATES.lock().unwrap_or_else(|poison| poison.into_inner());
    states.entry(receiver as usize).or_default().enabled = false;
    none()
}

unsafe extern "C" fn profiler_clear(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (receiver, args) = match unsafe { method_args(argv, argc, "clear") } {
        Ok(parts) => parts,
        Err(error) => return error,
    };
    if !args.is_empty() {
        return raise(ExceptionKind::TypeError, &format!("clear expected no arguments, got {}", args.len()));
    }
    let mut states = STATES.lock().unwrap_or_else(|poison| poison.into_inner());
    let state = states.entry(receiver as usize).or_default();
    state.clear_count = state.clear_count.wrapping_add(1);
    none()
}

unsafe extern "C" fn profiler_getstats(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let (receiver, args) = match unsafe { method_args(argv, argc, "getstats") } {
        Ok(parts) => parts,
        Err(error) => return error,
    };
    if !args.is_empty() {
        return raise(ExceptionKind::TypeError, &format!("getstats expected no arguments, got {}", args.len()));
    }
    let mut states = STATES.lock().unwrap_or_else(|poison| poison.into_inner());
    let state = states.entry(receiver as usize).or_default();
    let _observed_state = (state.enabled, state.subcalls, state.builtins, state.clear_count);
    // SAFETY: Passing a NULL data pointer with length zero builds a real empty list.
    unsafe { abi::seq::pon_build_list(ptr::null_mut(), 0) }
}

unsafe fn method_args<'a>(
    argv: *mut *mut PyObject,
    argc: usize,
    name: &str,
) -> Result<(*mut PyObject, &'a [*mut PyObject]), *mut PyObject> {
    if argv.is_null() || argc == 0 {
        return Err(raise(ExceptionKind::TypeError, &format!("{name} missing profiler receiver")));
    }
    let raw = unsafe { core::slice::from_raw_parts(argv.cast_const(), argc) };
    let receiver = crate::tag::untag_arg(raw[0]);
    if !is_profiler_receiver(receiver) {
        return Err(raise(ExceptionKind::TypeError, &format!("{name} receiver is not a Profiler")));
    }
    Ok((receiver, &raw[1..]))
}

fn is_profiler_receiver(receiver: *mut PyObject) -> bool {
    if receiver.is_null() || !crate::tag::is_heap(receiver) {
        return false;
    }
    let ty = unsafe { (*receiver).ob_type.cast_mut() };
    unsafe { crate::mro::is_subtype(ty, profiler_type()) }
}

fn optional_bool(value: Option<*mut PyObject>, default: bool) -> Option<bool> {
    value.map_or(Some(default), bool_arg)
}

fn bool_arg(value: *mut PyObject) -> Option<bool> {
    // SAFETY: Truthiness helper normalizes tagged immediates and reports -1 on error.
    match unsafe { abi::pon_is_true(crate::tag::untag_arg(value)) } {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

fn none() -> *mut PyObject {
    // SAFETY: Singleton accessor.
    unsafe { abi::pon_none() }
}

fn raise(kind: ExceptionKind, message: &str) -> *mut PyObject {
    crate::abi::exc::raise_kind_error_text(kind, message)
}
