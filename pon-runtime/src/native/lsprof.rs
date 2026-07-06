//! Native `_lsprof` compatibility surface.
//!
//! Pon records completed `PyFunction` call spans through the compiled-call stack
//! hooks in `abi::CurrentFunctionGuard`, using monotonic runtime timing.  The
//! `Profiler` object is stateful (`enable`, `disable`, `clear`) and
//! `getstats()` returns cProfile-compatible entry objects with `code`, call
//! counts, timings, and subcall lists.
//!
//! Custom timer/timeunit callbacks and low-level C call events are outside the
//! current product boundary; custom timers raise `NotImplementedError` instead
//! of being silently ignored.

use std::{collections::HashMap, ptr, sync::LazyLock};

use super::{builtins_mod::VARIADIC_ARITY, install_module};
use crate::{
	abi,
	intern::intern,
	object::{PyObject, PyObjectHeader, PyType},
	types::exc::ExceptionKind,
};

#[repr(C)]
struct PyProfilerEntry {
	ob_base:        PyObjectHeader,
	code:           *mut PyObject,
	callcount:      u64,
	reccallcount:   u64,
	total_seconds:  f64,
	inline_seconds: f64,
	calls:          *mut PyObject,
}


pub(super) fn make_module() -> Result<*mut PyObject, String> {
	let name = "_lsprof";
	let name_object =
		str_object(name).ok_or_else(|| "failed to allocate _lsprof.__name__".to_owned())?;
	let attrs = vec![
		(intern("__name__"), name_object),
		(intern("Profiler"), profiler_type().cast::<PyObject>()),
	];
	install_module(name, attrs)
}

static PROFILER_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"_lsprof.Profiler",
		core::mem::size_of::<crate::types::type_::PyHeapInstance>(),
	);
	ty.tp_base = abi::runtime_global(intern("object"))
		.map_or(ptr::null_mut(), |object| object.cast::<PyType>());
	ty.tp_dictoffset = 1;
	ty.tp_getattro = Some(profiler_getattro);
	ty.tp_setattro = Some(crate::descr::generic_set_attr);
	ty.tp_new = Some(crate::types::type_::type_new);
	ty.tp_init = Some(crate::types::type_::type_init);
	ty.gc_type_id = crate::types::type_::TYPE_ID_HEAP_INSTANCE.0 as usize;

	let namespace = crate::types::type_::new_namespace();
	set_str(
		namespace,
		"__doc__",
		"Profiler(timer=None, timeunit=None, subcalls=True, builtins=True)",
	);
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

const PROFILER_METHODS: &[(
	&str,
	unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
)] = &[
	("__init__", profiler_init),
	("__init_subclass__", profiler_init_subclass),
	("clear", profiler_clear),
	("create_stats", profiler_create_stats),
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

unsafe extern "C" fn profiler_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let Some(name_text) =
		(unsafe { crate::types::type_::unicode_text(crate::tag::untag_arg(name)) })
	else {
		return raise(ExceptionKind::TypeError, "attribute name must be str");
	};
	match name_text {
		"clear" => bound_method(object, name_text, profiler_clear),
		"create_stats" => bound_method(object, name_text, profiler_create_stats),
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
	let function =
		unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
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

	if let Some(&timer) = args.first() {
		if !is_none_object(timer) {
			return raise(
				ExceptionKind::NotImplementedError,
				"_lsprof.Profiler custom timer callbacks are not implemented in pon; the native \
				 profiler uses monotonic runtime timing",
			);
		}
	}
	if let Some(&timeunit) = args.get(1) {
		if !is_none_object(timeunit) {
			return raise(
				ExceptionKind::NotImplementedError,
				"_lsprof.Profiler timeunit is only meaningful with a custom timer, which pon does \
				 not implement",
			);
		}
	}
	let mut subcalls = true;
	let mut builtins = true;
	if let Some(&value) = args.get(2) {
		subcalls = match bool_arg(value) {
			Some(value) => value,
			None => return ptr::null_mut(),
		};
	}
	if let Some(&value) = args.get(3) {
		builtins = match bool_arg(value) {
			Some(value) => value,
			None => return ptr::null_mut(),
		};
	}

	abi::lsprof_register_profiler(receiver, subcalls, builtins);
	none()
}

unsafe extern "C" fn profiler_init_subclass(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	if argv.is_null() || argc == 0 {
		return raise(ExceptionKind::TypeError, "Profiler.__init_subclass__ missing class receiver");
	}
	if argc != 1 {
		return raise(
			ExceptionKind::TypeError,
			&format!("Profiler.__init_subclass__ expected no arguments, got {}", argc - 1),
		);
	}
	let cls = crate::tag::untag_arg(unsafe { *argv });
	if !is_type_object_pointer(cls) {
		return raise(ExceptionKind::TypeError, "Profiler.__init_subclass__ receiver is not a class");
	}
	if is_cprofile_profile_class(cls) {
		let ty = cls.cast::<PyType>();
		let namespace = unsafe { (*ty).tp_dict.cast::<crate::types::type_::PyClassDict>() };
		if !namespace.is_null() {
			set_function(namespace, "create_stats", profiler_create_stats);
			crate::sync::type_modified(ty);
		}
	}
	none()
}

unsafe extern "C" fn profiler_create_stats(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (receiver, args) = match unsafe { method_args(argv, argc, "create_stats") } {
		Ok(parts) => parts,
		Err(error) => return error,
	};
	if !args.is_empty() {
		return raise(
			ExceptionKind::TypeError,
			&format!("create_stats expected no arguments, got {}", args.len()),
		);
	}

	abi::lsprof_disable_profiler(receiver);

	let snapshot = unsafe { abi::pon_get_attr(receiver, intern("snapshot_stats"), ptr::null_mut()) };
	if snapshot.is_null() {
		return ptr::null_mut();
	}
	let result = unsafe { abi::pon_call(snapshot, ptr::null_mut(), 0) };
	if result.is_null() {
		return ptr::null_mut();
	}
	none()
}

unsafe extern "C" fn profiler_enable(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (receiver, args) = match unsafe { method_args(argv, argc, "enable") } {
		Ok(parts) => parts,
		Err(error) => return error,
	};
	if args.len() > 2 {
		return raise(
			ExceptionKind::TypeError,
			&format!("enable expected at most 2 arguments, got {}", args.len()),
		);
	}
	let subcalls = match optional_bool(args.first().copied(), true) {
		Some(value) => value,
		None => return ptr::null_mut(),
	};
	let builtins = match optional_bool(args.get(1).copied(), true) {
		Some(value) => value,
		None => return ptr::null_mut(),
	};

	abi::lsprof_enable_profiler(receiver, subcalls, builtins);
	none()
}

unsafe extern "C" fn profiler_disable(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (receiver, args) = match unsafe { method_args(argv, argc, "disable") } {
		Ok(parts) => parts,
		Err(error) => return error,
	};
	if !args.is_empty() {
		return raise(
			ExceptionKind::TypeError,
			&format!("disable expected no arguments, got {}", args.len()),
		);
	}
	abi::lsprof_disable_profiler(receiver);
	none()
}

unsafe extern "C" fn profiler_clear(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (receiver, args) = match unsafe { method_args(argv, argc, "clear") } {
		Ok(parts) => parts,
		Err(error) => return error,
	};
	if !args.is_empty() {
		return raise(
			ExceptionKind::TypeError,
			&format!("clear expected no arguments, got {}", args.len()),
		);
	}
	abi::lsprof_clear_profiler(receiver);
	none()
}

unsafe extern "C" fn profiler_getstats(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let (receiver, args) = match unsafe { method_args(argv, argc, "getstats") } {
		Ok(parts) => parts,
		Err(error) => return error,
	};
	if !args.is_empty() {
		return raise(
			ExceptionKind::TypeError,
			&format!("getstats expected no arguments, got {}", args.len()),
		);
	}
	let snapshots = abi::lsprof_stats_snapshot(receiver);
	let mut code_cache = HashMap::new();
	let mut entries = Vec::with_capacity(snapshots.len());
	for snapshot in snapshots {
		let entry = profiler_entry_from_snapshot(&snapshot, &mut code_cache);
		if entry.is_null() {
			return ptr::null_mut();
		}
		entries.push(entry);
	}
	build_list(entries)
}

static PROFILER_ENTRY_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"_lsprof.profiler_entry",
		core::mem::size_of::<PyProfilerEntry>(),
	);
	ty.tp_base = abi::runtime_global(intern("object"))
		.map_or(ptr::null_mut(), |object| object.cast::<PyType>());
	ty.tp_getattro = Some(profiler_entry_getattro);
	Box::into_raw(Box::new(ty)) as usize
});

fn profiler_entry_type() -> *mut PyType {
	*PROFILER_ENTRY_TYPE as *mut PyType
}

fn profiler_entry_from_snapshot(
	snapshot: &abi::LsprofEntrySnapshot,
	code_cache: &mut HashMap<usize, *mut PyObject>,
) -> *mut PyObject {
	let mut calls = Vec::with_capacity(snapshot.calls.len());
	for call in &snapshot.calls {
		let entry = profiler_entry_from_call(call, code_cache);
		if entry.is_null() {
			return ptr::null_mut();
		}
		calls.push(entry);
	}
	let calls = build_list(calls);
	if calls.is_null() {
		return ptr::null_mut();
	}
	let code = code_object_for_function(snapshot.function, code_cache);
	if code.is_null() {
		return ptr::null_mut();
	}
	alloc_profiler_entry(
		code,
		snapshot.callcount,
		snapshot.reccallcount,
		snapshot.total_seconds,
		snapshot.inline_seconds,
		calls,
	)
}

fn profiler_entry_from_call(
	call: &abi::LsprofCallSnapshot,
	code_cache: &mut HashMap<usize, *mut PyObject>,
) -> *mut PyObject {
	let calls = build_list(Vec::new());
	if calls.is_null() {
		return ptr::null_mut();
	}
	let code = code_object_for_function(call.function, code_cache);
	if code.is_null() {
		return ptr::null_mut();
	}
	alloc_profiler_entry(
		code,
		call.callcount,
		call.reccallcount,
		call.total_seconds,
		call.inline_seconds,
		calls,
	)
}

fn alloc_profiler_entry(
	code: *mut PyObject,
	callcount: u64,
	reccallcount: u64,
	total_seconds: f64,
	inline_seconds: f64,
	calls: *mut PyObject,
) -> *mut PyObject {
	Box::into_raw(Box::new(PyProfilerEntry {
		ob_base: PyObjectHeader::new(profiler_entry_type()),
		code,
		callcount,
		reccallcount,
		total_seconds,
		inline_seconds,
		calls,
	}))
	.cast::<PyObject>()
}

fn code_object_for_function(
	function: *mut PyObject,
	code_cache: &mut HashMap<usize, *mut PyObject>,
) -> *mut PyObject {
	let key = function as usize;
	if let Some(&code) = code_cache.get(&key) {
		return code;
	}
	if !function.is_null() {
		let code = unsafe { abi::pon_get_attr(function, intern("__code__"), ptr::null_mut()) };
		if !code.is_null() {
			code_cache.insert(key, code);
			return code;
		}
		crate::thread_state::pon_err_clear();
	}
	let code = str_object("<unknown profiled function>").unwrap_or(ptr::null_mut());
	if !code.is_null() {
		code_cache.insert(key, code);
	}
	code
}

unsafe extern "C" fn profiler_entry_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let Some(name_text) =
		(unsafe { crate::types::type_::unicode_text(crate::tag::untag_arg(name)) })
	else {
		return raise(ExceptionKind::TypeError, "attribute name must be str");
	};
	let entry = unsafe { &*object.cast::<PyProfilerEntry>() };
	match name_text {
		"code" => entry.code,
		"callcount" => count_object(entry.callcount),
		"reccallcount" => count_object(entry.reccallcount),
		"totaltime" => float_object(entry.total_seconds),
		"inlinetime" => float_object(entry.inline_seconds),
		"calls" => entry.calls,
		_ => unsafe { crate::abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
	}
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

fn is_type_object_pointer(object: *mut PyObject) -> bool {
	if object.is_null() || !crate::tag::is_heap(object) {
		return false;
	}
	let meta = unsafe { (*object).ob_type.cast_mut() };
	let type_type = abi::runtime_type_type();
	unsafe { meta == type_type || crate::mro::is_subtype(meta, type_type) }
}

fn is_cprofile_profile_class(object: *mut PyObject) -> bool {
	let ty = object.cast::<PyType>();
	if unsafe { (*ty).name() } != "Profile" {
		return false;
	}
	let dict = unsafe { (*ty).tp_dict.cast::<crate::types::type_::PyClassDict>() };
	if dict.is_null() {
		return false;
	}
	let Some(module) = (unsafe { (&*dict).get(intern("__module__")) }) else {
		return false;
	};
	unsafe { crate::types::type_::unicode_text(crate::tag::untag_arg(module)) == Some("cProfile") }
}

fn count_object(value: u64) -> *mut PyObject {
	let value = value.min(i64::MAX as u64) as i64;
	unsafe { abi::pon_const_int(value) }
}

fn float_object(value: f64) -> *mut PyObject {
	unsafe { abi::number::pon_const_float(value) }
}

fn build_list(mut items: Vec<*mut PyObject>) -> *mut PyObject {
	unsafe {
		abi::seq::pon_build_list(
			if items.is_empty() {
				ptr::null_mut()
			} else {
				items.as_mut_ptr()
			},
			items.len(),
		)
	}
}

fn is_none_object(value: *mut PyObject) -> bool {
	crate::tag::untag_arg(value) == none()
}

fn optional_bool(value: Option<*mut PyObject>, default: bool) -> Option<bool> {
	value.map_or(Some(default), bool_arg)
}

fn bool_arg(value: *mut PyObject) -> Option<bool> {
	// SAFETY: Truthiness helper normalizes tagged immediates and reports -1 on
	// error.
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
