//! CPython-source compatibility shim for recompiled native extensions.
//!
//! This is not CPython's binary ABI. Extensions include Pon's `Python.h`, link
//! the bootstrap object once, and the loader injects this process's function
//! tables before calling `PyInit_*`.
//!
//! Dispatch is grouped into per-family tables (see `include/pon_capi/*.h`);
//! the top-level [`PyPonCapi`] only aggregates family-table pointers plus a
//! `size` drift guard, so families evolve independently.

#[cfg(test)]
mod args_test;
mod containers;
mod err;
mod numbers;
mod object_;
mod runtime_;
mod strings;
pub(crate) mod twin;
mod typeobj;

use core::{
	ffi::{c_char, c_int, c_void},
	mem, ptr,
};
use std::{
	collections::{HashMap, HashSet},
	ffi::{CStr, CString},
	path::Path,
	sync::{LazyLock, Mutex, OnceLock},
};

pub(crate) use typeobj::is_capi_class;

use crate::{
	abi,
	intern::intern,
	object::{CallFunc, PyObject, PyObjectHeader, PyType, as_object_ptr},
	thread_state::{pon_err_message, pon_err_occurred, pon_err_set},
};

const METH_VARARGS: c_int = 0x0001;
const METH_KEYWORDS: c_int = 0x0002;
/// Compile-time paths of the C-API source shim: the `Python.h` include root
/// plus the bootstrap and argument-parser translation units every extension
/// links. Package-manager build flows (meson) compile these once and inject
/// the objects through the linker environment.
#[must_use]
pub fn capi_shim_paths() -> (&'static str, &'static str, &'static str) {
	(
		concat!(env!("CARGO_MANIFEST_DIR"), "/include"),
		concat!(env!("CARGO_MANIFEST_DIR"), "/capi/pon_capi_bootstrap.c"),
		concat!(env!("CARGO_MANIFEST_DIR"), "/capi/pon_capi_args.c"),
	)
}
const METH_NOARGS: c_int = 0x0004;
const METH_O: c_int = 0x0008;
const METH_CLASS: c_int = 0x0010;
const METH_STATIC: c_int = 0x0020;
const METH_FASTCALL: c_int = 0x0080;

const PYTHON_API_VERSION: c_int = 1013;
const PY_MOD_CREATE: c_int = 1;
const PY_MOD_EXEC: c_int = 2;
const PY_MOD_MULTIPLE_INTERPRETERS: c_int = 3;
const PY_MOD_GIL: c_int = 4;

/// C signature used by classic `PyMethodDef` function entries.
pub type PyCFunction = unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject;

type PyPonSetCapi = unsafe extern "C" fn(*const PyPonCapi) -> c_int;
type PyInitFunc = unsafe extern "C" fn() -> *mut PyObject;

/// Minimal `PyMethodDef` layout consumed by [`PyModuleDef`].
#[repr(C)]
pub struct PyMethodDef {
	/// NUL-terminated Python attribute name.
	pub ml_name:  *const c_char,
	/// C callable implementing the method.
	pub ml_meth:  Option<PyCFunction>,
	/// CPython `METH_*` flag mask.
	pub ml_flags: c_int,
	/// Optional NUL-terminated docstring.
	pub ml_doc:   *const c_char,
}

/// Prefix used by CPython's `PyModuleDef_HEAD_INIT` initializer.
#[repr(C)]
pub struct PyModuleDefBase {
	ob_base: PyObjectHeader,
	m_init:  *mut c_void,
	m_index: isize,
	m_copy:  *mut PyObject,
}
/// CPython multi-phase module slot descriptor (`moduleobject.h`).
#[repr(C)]
pub struct PyModuleDefSlot {
	slot:  c_int,
	value: *mut c_void,
}

/// Minimal module definition accepted by `PyModule_Create2`/`PyModuleDef_Init`.
#[repr(C)]
pub struct PyModuleDef {
	base:       PyModuleDefBase,
	m_name:     *const c_char,
	m_doc:      *const c_char,
	m_size:     isize,
	m_methods:  *const PyMethodDef,
	m_slots:    *mut PyModuleDefSlot,
	m_traverse: *mut c_void,
	m_clear:    *mut c_void,
	m_free:     *mut c_void,
}

/// Function-table hub injected into recompiled extension modules.
///
/// `size` guards layout drift at load time; the bootstrap rejects a table
/// whose size differs from the header it was compiled against. Family
/// pointers only: append new families at the end, never reorder.
#[repr(C)]
pub struct PyPonCapi {
	size:       usize,
	core:       *const PyPonCapiCore,
	err:        *const err::PyPonCapiErr,
	numbers:    *const numbers::PyPonCapiNumbers,
	strings:    *const strings::PyPonCapiStrings,
	containers: *const containers::PyPonCapiContainers,
	runtime_:   *const runtime_::PyPonCapiRuntime,
	object_:    *const object_::PyPonCapiObject,
	typeobj:    *const typeobj::PyPonCapiTypeObj,
}

unsafe impl Sync for PyPonCapi {}
unsafe impl Send for PyPonCapi {}

/// C mirror: `include/pon_capi/core.h` `PyPonCapiCore`.
#[repr(C)]
struct PyPonCapiCore {
	module_create2:       unsafe extern "C" fn(*mut PyModuleDef, c_int) -> *mut PyObject,
	module_add_object:    unsafe extern "C" fn(*mut PyObject, *const c_char, *mut PyObject) -> c_int,
	inc_ref:              unsafe extern "C" fn(*mut PyObject),
	dec_ref:              unsafe extern "C" fn(*mut PyObject),
	none:                 unsafe extern "C" fn() -> *mut PyObject,
	bool_true:            unsafe extern "C" fn() -> *mut PyObject,
	bool_false:           unsafe extern "C" fn() -> *mut PyObject,
	not_implemented:      unsafe extern "C" fn() -> *mut PyObject,
	register_local_twins: unsafe extern "C" fn(*const *mut twin::ForeignTypeObject, c_int) -> c_int,
	builtin_type_id:      unsafe extern "C" fn(*mut PyObject) -> c_int,
	foreign_of:           unsafe extern "C" fn(*mut PyObject) -> *mut twin::ForeignTypeObject,
	ellipsis:             unsafe extern "C" fn() -> *mut PyObject,
	normalize_foreign:    unsafe extern "C" fn(*mut PyObject) -> *mut PyObject,
}

unsafe impl Sync for PyPonCapiCore {}
unsafe impl Send for PyPonCapiCore {}

#[repr(C)]
struct PyCFunctionObject {
	ob_base:       PyObjectHeader,
	m_ml:          *mut PyMethodDef,
	m_self:        *mut PyObject,
	m_module:      *mut PyObject,
	m_weakreflist: *mut PyObject,
	vectorcall:    *mut c_void,
	name:          u32,
}

const CAPI_CFUNCTION_WORD: usize = mem::size_of::<*mut c_void>();
const _: () = assert!(mem::size_of::<PyObjectHeader>() == 2 * CAPI_CFUNCTION_WORD);
const _: () = assert!(mem::offset_of!(PyCFunctionObject, ob_base) == 0);
const _: () = assert!(mem::offset_of!(PyCFunctionObject, m_ml) == 2 * CAPI_CFUNCTION_WORD);
const _: () = assert!(mem::offset_of!(PyCFunctionObject, m_self) == 3 * CAPI_CFUNCTION_WORD);
const _: () = assert!(mem::offset_of!(PyCFunctionObject, m_module) == 4 * CAPI_CFUNCTION_WORD);
const _: () = assert!(mem::offset_of!(PyCFunctionObject, m_weakreflist) == 5 * CAPI_CFUNCTION_WORD);
const _: () = assert!(mem::offset_of!(PyCFunctionObject, vectorcall) == 6 * CAPI_CFUNCTION_WORD);
const _: () = assert!(mem::offset_of!(PyCFunctionObject, name) == 7 * CAPI_CFUNCTION_WORD);

/// GC type id for C-function carriers (registry: pon-gc ids live in
/// `abi::register_gc_types` and per-module constants; 141 is next to the
/// native-file id 120 and capi-instance id 140).
const TYPE_ID_CAPI_CFUNCTION: pon_gc::TypeId = pon_gc::TypeId(141);

/// Traces the bound receiver so a carrier can never outlive it.
///
/// # Safety
///
/// `object` points to a live `PyCFunctionObject` allocation.
unsafe extern "C" fn trace_cfunction(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
	if object.is_null() {
		return;
	}
	// SAFETY: caller contract — live carrier allocation.
	let function = unsafe { &*object.cast::<PyCFunctionObject>() };
	for field in [function.m_self, function.m_module, function.m_weakreflist] {
		if !field.is_null() && crate::tag::is_heap(field.cast()) {
			visitor(field.cast());
		}
	}
}

/// Carrier attribute assignment. CPython's `PyCFunctionObject` accepts
/// `__doc__` (numpy's `add_docstring` writes `m_ml->ml_doc`; we mirror with
/// a leaked utf-8 copy) and `__module__` (a writable member backed by
/// `m_module` — numpy's `multiarray.py` re-homes ~30 C functions).
/// Every other attribute keeps rejecting assignment like CPython.
unsafe extern "C" fn cfunction_setattro(
	object: *mut PyObject,
	name: *mut PyObject,
	value: *mut PyObject,
) -> c_int {
	let attr = unsafe { crate::types::type_::unicode_text(name) };
	// SAFETY: carrier layout per C_FUNCTION_TYPE; m_ml is process-lifetime.
	let function = unsafe { &mut *object.cast::<PyCFunctionObject>() };
	match attr {
		Some("__doc__") if !value.is_null() => {
			let Some(text) = (unsafe { crate::types::type_::unicode_text(value) }) else {
				let _ = abi::return_null_with_type_error("__doc__ must be a str");
				return -1;
			};
			let Ok(c_text) = CString::new(text) else {
				let _ = abi::return_null_with_type_error("__doc__ contains NUL");
				return -1;
			};
			if let Some(method_def) = unsafe { function.m_ml.as_mut() } {
				method_def.ml_doc = c_text.into_raw();
			}
			0
		},
		Some("__module__") if !value.is_null() => {
			// Traced via `trace_cfunction` so the stored module name stays live.
			function.m_module = value;
			0
		},
		_ => {
			let _ = abi::return_null_with_type_error("object does not support attribute assignment");
			-1
		},
	}
}

/// Carrier attribute reads: CPython's `builtin_function_or_method` exposes
/// `__name__`/`__qualname__` (ml_name), `__doc__` (ml_doc or None),
/// `__module__` (m_module or None) and `__self__` (m_self or None); numpy's
/// `_ArrayFunctionDispatcher` C init reads them unguarded. Everything else
/// delegates to the generic attribute machinery.
unsafe extern "C" fn cfunction_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let attr = unsafe { crate::types::type_::unicode_text(name) };
	// SAFETY: dispatch reaches here only for live carrier instances.
	let function = unsafe { &*object.cast::<PyCFunctionObject>() };
	let method_def = unsafe { function.m_ml.as_ref() };
	match attr {
		Some("__name__") | Some("__qualname__") => {
			let raw = method_def.map_or(ptr::null(), |def| def.ml_name);
			if raw.is_null() {
				return unsafe { abi::pon_const_str("<builtin>".as_ptr(), "<builtin>".len()) };
			}
			let text = unsafe { CStr::from_ptr(raw) }.to_string_lossy();
			unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
		},
		Some("__doc__") => {
			let raw = method_def.map_or(ptr::null(), |def| def.ml_doc);
			if raw.is_null() {
				return unsafe { abi::pon_none() };
			}
			let text = unsafe { CStr::from_ptr(raw) }.to_string_lossy();
			unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
		},
		Some("__module__") => {
			if function.m_module.is_null() {
				unsafe { abi::pon_none() }
			} else {
				function.m_module
			}
		},
		Some("__self__") => {
			if function.m_self.is_null() {
				unsafe { abi::pon_none() }
			} else {
				function.m_self
			}
		},
		_ => unsafe { crate::descr::generic_get_attr(object, name) },
	}
}

static C_FUNCTION_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty =
		PyType::new(ptr::null(), "builtin_function_or_method", mem::size_of::<PyCFunctionObject>());
	ty.tp_call = Some(cfunction_call as CallFunc);
	// C methods installed in a Ready'd type's namespace bind their receiver
	// through the descriptor protocol (METH_CLASS binds the type,
	// METH_STATIC stays unbound).
	ty.tp_descr_get = Some(cfunction_descr_get);
	ty.tp_setattro = Some(cfunction_setattro);
	ty.tp_getattro = Some(cfunction_getattro);
	Box::into_raw(Box::new(ty)) as usize
});

static CAPI_PINS: LazyLock<Mutex<HashMap<usize, usize>>> =
	LazyLock::new(|| Mutex::new(HashMap::new()));
static EXTENSION_HANDLES: LazyLock<Mutex<Vec<usize>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static MODULE_DEF_REGISTRY: LazyLock<Mutex<HashSet<usize>>> =
	LazyLock::new(|| Mutex::new(HashSet::new()));

/// Owns every family table. Built on first extension load: the err family
/// fabricates `PyExc_*` twins and therefore requires an initialized runtime
/// (`OnceLock`, not `LazyLock`: runtime input).
struct Families {
	core:       PyPonCapiCore,
	err:        err::PyPonCapiErr,
	numbers:    numbers::PyPonCapiNumbers,
	strings:    strings::PyPonCapiStrings,
	containers: containers::PyPonCapiContainers,
	runtime_:   runtime_::PyPonCapiRuntime,
	object_:    object_::PyPonCapiObject,
	typeobj:    typeobj::PyPonCapiTypeObj,
}

static FAMILIES: OnceLock<Families> = OnceLock::new();
static CAPI: OnceLock<PyPonCapi> = OnceLock::new();

/// Assembles (once) and returns the process-lifetime injected table.
fn capi_table() -> *const PyPonCapi {
	let families = FAMILIES.get_or_init(|| Families {
		core:       PyPonCapiCore {
			module_create2:       py_module_create2,
			module_add_object:    py_module_add_object,
			inc_ref:              py_inc_ref,
			dec_ref:              py_dec_ref,
			none:                 py_none,
			bool_true:            py_true,
			bool_false:           py_false,
			not_implemented:      py_not_implemented,
			register_local_twins: twin::capi_register_local_twins,
			builtin_type_id:      twin::capi_builtin_type_id,
			foreign_of:           twin::capi_foreign_of,
			ellipsis:             py_ellipsis,
			normalize_foreign:    py_normalize_foreign,
		},
		err:        err::build(),
		numbers:    numbers::build(),
		strings:    strings::build(),
		containers: containers::build(),
		runtime_:   runtime_::build(),
		object_:    object_::build(),
		typeobj:    typeobj::build(),
	});
	CAPI.get_or_init(|| PyPonCapi {
		size:       mem::size_of::<PyPonCapi>(),
		core:       &families.core,
		err:        &families.err,
		numbers:    &families.numbers,
		strings:    &families.strings,
		containers: &families.containers,
		runtime_:   &families.runtime_,
		object_:    &families.object_,
		typeobj:    &families.typeobj,
	})
}

/// Extension suffixes Pon will consider for source-recompiled modules.
#[must_use]
pub fn extension_suffixes() -> &'static [&'static str] {
	#[cfg(target_os = "macos")]
	{
		&[".pon.so", ".cpython-314-darwin.so", ".abi3.so", ".so"]
	}
	#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
	{
		&[".pon.so", ".cpython-314-x86_64-linux-gnu.so", ".abi3.so", ".so"]
	}
	#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
	{
		&[".pon.so", ".cpython-314-aarch64-linux-gnu.so", ".abi3.so", ".so"]
	}
	#[cfg(not(any(
		target_os = "macos",
		all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))
	)))]
	{
		&[".pon.so", ".so"]
	}
}

/// Current C-extension pins exposed to the collector as explicit roots.
#[must_use]
pub(crate) fn gc_held_roots() -> Vec<*mut PyObject> {
	CAPI_PINS
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.keys()
		.copied()
		.filter(|&addr| addr >= 4096)
		.map(|addr| addr as *mut PyObject)
		.filter(|&object| crate::tag::is_heap(object))
		.collect()
}

/// Loads a source-recompiled extension module and calls its `PyInit_*` entry.
pub(crate) fn load_extension_module(name: &str, path: &Path) -> Result<*mut PyObject, String> {
	let path_text = path
		.to_str()
		.ok_or_else(|| format!("extension path is not UTF-8: {}", path.display()))?;
	let c_path = CString::new(path_text)
		.map_err(|_| format!("extension path contains NUL: {}", path.display()))?;
	let handle = unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
	if handle.is_null() {
		return Err(format!("failed to load extension '{}': {}", path.display(), dlerror_text()));
	}

	let set_capi = unsafe { symbol::<PyPonSetCapi>(handle, "PyPon_SetCapi") }?;
	let set_result = unsafe { set_capi(capi_table()) };
	if set_result != 0 {
		unsafe { libc::dlclose(handle) };
		return Err(format!("extension '{}' rejected Pon C API table", path.display()));
	}

	let short_name = name.rsplit('.').next().unwrap_or(name);
	let init_symbol = format!("PyInit_{short_name}");
	let init = unsafe { symbol::<PyInitFunc>(handle, &init_symbol) }?;
	let init_result = unsafe { init() };
	if init_result.is_null() {
		let message = if pon_err_occurred() {
			pon_err_message().unwrap_or_else(|| "extension init failed".to_owned())
		} else {
			"extension init returned NULL without setting an exception".to_owned()
		};
		unsafe { libc::dlclose(handle) };
		return Err(message);
	}

	let module = if let Some(def) = registered_module_def(init_result) {
		match unsafe { load_multi_phase_extension_module(def, name) } {
			Ok(module) => module,
			Err(message) => {
				unsafe { libc::dlclose(handle) };
				return Err(message);
			},
		}
	} else {
		// PyInit_* returns a new reference to the loader.  Once the module is
		// installed in Pon's import registry, transfer that C-owned reference
		// back to the GC-managed runtime.
		unpin_object(init_result);
		init_result
	};

	EXTENSION_HANDLES
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.push(handle as usize);
	Ok(module)
}

unsafe fn symbol<T: Copy>(handle: *mut c_void, name: &str) -> Result<T, String> {
	let c_name = CString::new(name).map_err(|_| format!("symbol name contains NUL: {name}"))?;
	let ptr = unsafe { libc::dlsym(handle, c_name.as_ptr()) };
	if ptr.is_null() {
		return Err(format!("missing extension symbol '{name}': {}", dlerror_text()));
	}
	Ok(unsafe { mem::transmute_copy(&ptr) })
}

fn dlerror_text() -> String {
	let error = unsafe { libc::dlerror() };
	if error.is_null() {
		"unknown dynamic loader error".to_owned()
	} else {
		unsafe { CStr::from_ptr(error) }
			.to_string_lossy()
			.into_owned()
	}
}

pub(super) unsafe extern "C" fn py_module_def_init(def: *mut PyModuleDef) -> *mut PyObject {
	if def.is_null() {
		return abi::return_null_with_error("PyModuleDef_Init received NULL PyModuleDef");
	}
	MODULE_DEF_REGISTRY
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.insert(def as usize);
	def.cast::<PyObject>()
}

fn registered_module_def(object: *mut PyObject) -> Option<*mut PyModuleDef> {
	if object.is_null() {
		return None;
	}
	MODULE_DEF_REGISTRY
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.contains(&(object as usize))
		.then_some(object.cast::<PyModuleDef>())
}

unsafe fn load_multi_phase_extension_module(
	def: *mut PyModuleDef,
	import_name: &str,
) -> Result<*mut PyObject, String> {
	let def_ref = unsafe { &*def };
	let Some(name) = c_string(def_ref.m_name) else {
		let message = "module definition has no name".to_owned();
		pon_err_set(message.clone());
		return Err(message);
	};
	let module = unsafe { create_module_from_def(def_ref, false) };
	if module.is_null() {
		return Err(pending_error_message("multi-phase module creation failed".to_owned()));
	}
	if def_ref.m_size >= 0
		&& let Err(message) = runtime_::register_module_state(module, def_ref.m_size as usize)
	{
		pon_err_set(message.clone());
		return Err(message);
	}
	// CPython installs the module in `sys.modules` BEFORE exec so exec-time
	// self re-imports adopt it (numpy hard-errors on double exec).
	if let Err(message) = crate::import::register_extension_module_for_exec(import_name, module) {
		runtime_::unregister_module_state(module);
		pon_err_set(message.clone());
		return Err(message);
	}
	// Slots run with the module ACTIVE so C writes through the module dict
	// (PyModule_GetDict + PyDict_SetItemString — numpy installs every ufunc
	// this way) mirror into the module's attr registry, which star-import
	// and pon-side getattr iterate.
	if let Err(message) = crate::import::begin_module_execution(&name) {
		crate::import::unregister_extension_module_after_failed_exec(import_name, module);
		runtime_::unregister_module_state(module);
		pon_err_set(message.clone());
		return Err(message);
	}
	let slots_result = unsafe { run_module_slots(module, def_ref, &name) };
	crate::import::end_module_execution(&name);
	if let Err(message) = slots_result {
		crate::import::unregister_extension_module_after_failed_exec(import_name, module);
		runtime_::unregister_module_state(module);
		return Err(message);
	}
	Ok(module)
}

unsafe extern "C" fn py_module_create2(def: *mut PyModuleDef, api_version: c_int) -> *mut PyObject {
	if api_version != PYTHON_API_VERSION || def.is_null() {
		return abi::return_null_with_error("invalid PyModuleDef");
	}
	let def_ref = unsafe { &*def };
	pin_new_reference(unsafe { create_module_from_def(def_ref, true) })
}

unsafe fn create_module_from_def(def_ref: &PyModuleDef, reject_slots: bool) -> *mut PyObject {
	if reject_slots && !def_ref.m_slots.is_null() {
		return abi::return_null_with_error("multi-phase extension modules are not supported yet");
	}
	let Some(name) = c_string(def_ref.m_name) else {
		return abi::return_null_with_error("module definition has no name");
	};
	let mut attrs = Vec::new();
	if let Some(doc) = c_string(def_ref.m_doc) {
		let doc_object = unsafe { abi::pon_const_str(doc.as_ptr(), doc.len()) };
		if doc_object.is_null() {
			return ptr::null_mut();
		}
		attrs.push((intern("__doc__"), doc_object));
	}
	if !def_ref.m_methods.is_null() {
		let mut cursor = def_ref.m_methods;
		loop {
			let method = unsafe { &*cursor };
			if method.ml_name.is_null() {
				break;
			}
			let Some(method_name) = c_string(method.ml_name) else {
				return abi::return_null_with_error("method definition has invalid name");
			};
			if method.ml_meth.is_none() {
				return abi::return_null_with_error(format!("method '{method_name}' has no function"));
			}
			let object =
				alloc_cfunction_from_method_def(cursor.cast_mut(), ptr::null_mut(), &method_name);
			attrs.push((intern(&method_name), object));
			cursor = unsafe { cursor.add(1) };
		}
	}
	match crate::import::install_module(&name, attrs) {
		Ok(module) => module,
		Err(message) => abi::return_null_with_error(message),
	}
}

unsafe fn run_module_slots(
	module: *mut PyObject,
	def_ref: &PyModuleDef,
	module_name: &str,
) -> Result<(), String> {
	if def_ref.m_slots.is_null() {
		return Ok(());
	}
	let mut cursor = def_ref.m_slots;
	loop {
		let slot = unsafe { &*cursor };
		match slot.slot {
			0 => return Ok(()),
			PY_MOD_CREATE => {
				// PEP 489 custom module creation.  The dominant emitter
				// (Cython's `__pyx_pymod_create`) builds exactly the default
				// module and copies spec attributes onto it; the runtime
				// already created that module and owns its registration, so
				// the hook is skipped rather than called with a synthetic
				// spec (numpy.random's Cython extensions load this way).
			},
			PY_MOD_EXEC => {
				if slot.value.is_null() {
					return module_load_error(format!(
						"module '{module_name}' has NULL Py_mod_exec slot"
					));
				}
				let exec: unsafe extern "C" fn(*mut PyObject) -> c_int =
					unsafe { mem::transmute(slot.value) };
				if unsafe { exec(module) } != 0 {
					if pon_err_occurred() {
						return Err(pon_err_message().unwrap_or_else(|| {
							format!("Py_mod_exec slot for module '{module_name}' failed")
						}));
					}
					return module_load_error(format!(
						"Py_mod_exec slot for module '{module_name}' failed without setting an exception"
					));
				}
			},
			PY_MOD_MULTIPLE_INTERPRETERS | PY_MOD_GIL => {
				// Pon currently has one interpreter and no GIL. These CPython
				// declarations are accepted for source compatibility and need
				// no runtime work under Pon's execution model.
			},
			other => {
				return module_load_error(format!(
					"module '{module_name}' has unsupported PyModuleDef slot {other}"
				));
			},
		}
		cursor = unsafe { cursor.add(1) };
	}
}

fn module_load_error<T>(message: String) -> Result<T, String> {
	pon_err_set(message.clone());
	Err(message)
}

fn pending_error_message(default: String) -> String {
	if pon_err_occurred() {
		pon_err_message().unwrap_or(default)
	} else {
		default
	}
}

unsafe extern "C" fn py_module_add_object(
	module: *mut PyObject,
	name: *const c_char,
	value: *mut PyObject,
) -> c_int {
	if module.is_null() || value.is_null() {
		pon_err_set("PyModule_AddObject received NULL".to_owned());
		return -1;
	}
	// Extensions publish their static types as module attributes
	// (`PyModule_AddObject(m, "Counter", (PyObject *)&CounterType)`); foreign
	// statics must never enter the runtime object graph — swap in the native
	// type they were Ready'd into.
	let original_value = value;
	let value = match twin::registered_native_of_foreign(value.cast::<twin::ForeignTypeObject>()) {
		Some(native) => native.cast::<PyObject>(),
		None => value,
	};
	let Some(attr) = c_string(name) else {
		pon_err_set("PyModule_AddObject name is not valid UTF-8".to_owned());
		return -1;
	};
	let module = module.cast::<crate::import::PyModuleObject>();
	let module_name = unsafe { (*module).name };
	if crate::import::store_module_attr(module_name, intern(&attr), value) {
		unpin_object(original_value);
		0
	} else {
		pon_err_set(format!("PyModule_AddObject target is not a module for '{attr}'"));
		-1
	}
}

unsafe extern "C" fn py_true() -> *mut PyObject {
	crate::types::bool_::from_bool(true)
}

unsafe extern "C" fn py_false() -> *mut PyObject {
	crate::types::bool_::from_bool(false)
}

unsafe extern "C" fn py_not_implemented() -> *mut PyObject {
	unsafe { abi::pon_not_implemented() }
}

/// Pins `object` as an explicit GC root (C-side owned reference); counted,
/// so pins nest. No-op for NULL, sentinel low addresses, and immediates.
pub(super) fn pin_object(object: *mut PyObject) {
	if object.is_null() || object.addr() < 4096 || !crate::tag::is_heap(object) {
		return;
	}
	let mut pins = CAPI_PINS
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());
	*pins.entry(object as usize).or_insert(0) += 1;
}

/// Twin contract, reverse leg: native type objects never cross into C raw —
/// a registered native type returned by any table function is translated to
/// its foreign face (address-keyed registry probe; no deref for non-types).
pub(super) fn foreignize_type_result(object: *mut PyObject) -> *mut PyObject {
	if object.is_null() || !crate::tag::is_heap(object) {
		return object;
	}
	match twin::registered_foreign_of_native(object.cast::<PyType>()) {
		Some(foreign) => foreign.cast::<PyObject>(),
		None => object,
	}
}

pub(super) fn pin_new_reference(object: *mut PyObject) -> *mut PyObject {
	// Pin the NATIVE object (faces are C statics `pin_object` ignores), then
	// hand C the face. The pin/unpin asymmetry is harmless: only registered
	// TYPE objects translate, and those are process-lifetime.
	pin_object(object);
	foreignize_type_result(object)
}

pub(super) fn unpin_object(object: *mut PyObject) {
	if object.is_null() || object.addr() < 4096 || !crate::tag::is_heap(object) {
		return;
	}
	let mut pins = CAPI_PINS
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());
	if let Some(count) = pins.get_mut(&(object as usize)) {
		*count = count.saturating_sub(1);
		if *count == 0 {
			pins.remove(&(object as usize));
		}
	}
}

#[cfg(test)]
pub(super) fn pin_count(object: *mut PyObject) -> usize {
	if object.is_null() || object.addr() < 4096 || !crate::tag::is_heap(object) {
		return 0;
	}
	CAPI_PINS
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.get(&(object as usize))
		.copied()
		.unwrap_or(0)
}

unsafe extern "C" fn py_inc_ref(object: *mut PyObject) {
	pin_object(object);
}

unsafe extern "C" fn py_dec_ref(object: *mut PyObject) {
	unpin_object(object);
}

unsafe extern "C" fn py_none() -> *mut PyObject {
	unsafe { abi::pon_none() }
}

unsafe extern "C" fn py_ellipsis() -> *mut PyObject {
	unsafe { abi::pon_ellipsis() }
}

/// Foreign-face -> native translation for C code that stores objects into
/// pon structures (`Py_BuildValue` "O"/"S"/"N"): registered foreign type
/// statics collapse onto their native class; everything else passes through.
unsafe extern "C" fn py_normalize_foreign(object: *mut PyObject) -> *mut PyObject {
	twin::registered_native_of_foreign(object.cast::<twin::ForeignTypeObject>())
		.map_or(object, |native| native.cast::<PyObject>())
}

unsafe extern "C" fn cfunction_call(
	callee: *mut PyObject,
	args: *mut PyObject,
	_kwargs: *mut PyObject,
) -> *mut PyObject {
	if callee.is_null() {
		return abi::return_null_with_error("NULL C function object");
	}
	let function = unsafe { &*callee.cast::<PyCFunctionObject>() };
	let Some(method_def) = (unsafe { function.m_ml.as_ref() }) else {
		return abi::return_null_with_error("C function object has no method definition");
	};
	let Some(method) = method_def.ml_meth else {
		return abi::return_null_with_error("C function object has no function pointer");
	};
	let flags = method_def.ml_flags;
	// Twin contract, inbound-to-C leg: registered native TYPE objects never
	// cross into C raw — C code casts arguments to CPython struct layouts
	// (numpy's add_docstring reads `((PyTypeObject *)obj)->tp_doc`).
	let self_object = foreignize_type_result(function.m_self);
	let positional: Vec<*mut PyObject> = match unsafe { tuple_args(args) } {
		Ok(values) => values
			.iter()
			.map(|&value| foreignize_type_result(value))
			.collect(),
		Err(message) => return abi::return_null_with_error(message),
	};
	// C sees a fresh tuple of translated operands for the tuple conventions;
	// pinned across the call (only C references it while it runs).
	let build_call_tuple = |values: &[*mut PyObject]| -> *mut PyObject {
		let mut values = values.to_vec();
		let tuple = unsafe { abi::seq::pon_build_tuple(values.as_mut_ptr(), values.len()) };
		pin_object(tuple);
		tuple
	};
	// Keyword operands snapshot: tp_call hands kwargs as a dict (or NULL).
	// The tuple conventions forward the dict itself; the fastcall
	// convention flattens it into `argv[nargs..]` + a kwnames tuple below.
	let kwarg_entries: Vec<crate::types::dict::DictEntry> = if _kwargs.is_null() {
		Vec::new()
	} else {
		match unsafe { crate::types::dict::dict_entries_snapshot(crate::tag::untag_arg(_kwargs)) } {
			Ok(entries) => entries,
			Err(message) => return abi::return_null_with_error(message),
		}
	};
	// CPython: conventions without METH_KEYWORDS reject keyword operands
	// instead of silently dropping them.
	if !kwarg_entries.is_empty() && flags & METH_KEYWORDS == 0 {
		return abi::return_null_with_error(format!(
			"{}() takes no keyword arguments",
			crate::intern::resolve(function.name).unwrap_or_default()
		));
	}
	if flags & METH_KEYWORDS != 0 && flags & METH_FASTCALL == 0 {
		// METH_VARARGS|METH_KEYWORDS: (self, args_tuple, kwargs_dict_or_NULL).
		let with_keywords: unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject =
            // SAFETY: the METH_KEYWORDS flag certifies the C entry was
            // declared with the PyCFunctionWithKeywords signature.
            unsafe { mem::transmute(method) };
		let tuple = build_call_tuple(&positional);
		if tuple.is_null() {
			return ptr::null_mut();
		}
		let result = unsafe { with_keywords(self_object, tuple, _kwargs) };
		unpin_object(tuple);
		unpin_object(result);
		// Faces echoed by C never re-enter pon raw (twin contract).
		return unsafe { py_normalize_foreign(result) };
	}
	if flags & METH_FASTCALL != 0 {
		if flags & METH_KEYWORDS != 0 {
			let fastcall_kw: unsafe extern "C" fn(*mut PyObject, *const *mut PyObject, isize, *mut PyObject) -> *mut PyObject =
                // SAFETY: METH_FASTCALL|METH_KEYWORDS certifies the
                // _PyCFunctionFastWithKeywords signature.
                unsafe { mem::transmute(method) };
			// _PyCFunctionFastWithKeywords vectorcall shape: argv holds the
			// positionals followed by the keyword VALUES, `nargs` counts the
			// positionals only, and kwnames is a tuple of the keyword name
			// strings (NULL when no keywords).
			let mut argv = positional.clone();
			let mut kwnames = ptr::null_mut();
			if !kwarg_entries.is_empty() {
				let mut names: Vec<*mut PyObject> = Vec::with_capacity(kwarg_entries.len());
				for entry in &kwarg_entries {
					names.push(foreignize_type_result(entry.key));
					argv.push(foreignize_type_result(entry.value));
				}
				kwnames = unsafe { abi::seq::pon_build_tuple(names.as_mut_ptr(), names.len()) };
				if kwnames.is_null() {
					return ptr::null_mut();
				}
				pin_object(kwnames);
			}
			let result =
				unsafe { fastcall_kw(self_object, argv.as_ptr(), positional.len() as isize, kwnames) };
			if !kwnames.is_null() {
				unpin_object(kwnames);
			}
			unpin_object(result);
			// Faces echoed by C never re-enter pon raw (twin contract).
			return unsafe { py_normalize_foreign(result) };
		}
		let fastcall: unsafe extern "C" fn(*mut PyObject, *const *mut PyObject, isize) -> *mut PyObject =
            // SAFETY: METH_FASTCALL certifies the _PyCFunctionFast signature.
            unsafe { mem::transmute(method) };
		let result = unsafe { fastcall(self_object, positional.as_ptr(), positional.len() as isize) };
		unpin_object(result);
		// Faces echoed by C never re-enter pon raw (twin contract).
		return unsafe { py_normalize_foreign(result) };
	}
	if flags & METH_NOARGS != 0 {
		if !positional.is_empty() {
			return abi::return_null_with_error(format!(
				"{}() takes no arguments",
				crate::intern::resolve(function.name).unwrap_or_default()
			));
		}
		let result = unsafe { method(self_object, ptr::null_mut()) };
		unpin_object(result);
		// Faces echoed by C never re-enter pon raw (twin contract).
		return unsafe { py_normalize_foreign(result) };
	}
	if flags & METH_O != 0 {
		if positional.len() != 1 {
			return abi::return_null_with_error(format!(
				"{}() takes exactly one argument",
				crate::intern::resolve(function.name).unwrap_or_default()
			));
		}
		let result = unsafe { method(self_object, positional[0]) };
		unpin_object(result);
		// Faces echoed by C never re-enter pon raw (twin contract).
		return unsafe { py_normalize_foreign(result) };
	}
	if flags & METH_VARARGS != 0 {
		let tuple = build_call_tuple(&positional);
		if tuple.is_null() {
			return ptr::null_mut();
		}
		let result = unsafe { method(self_object, tuple) };
		unpin_object(tuple);
		unpin_object(result);
		// Faces echoed by C never re-enter pon raw (twin contract).
		return unsafe { py_normalize_foreign(result) };
	}
	abi::return_null_with_error("unsupported C function calling convention")
}

unsafe fn tuple_args<'a>(args: *mut PyObject) -> Result<&'a [*mut PyObject], String> {
	if args.is_null() {
		return Ok(&[]);
	}
	unsafe { crate::abi::seq::exact_tuple_slice(args) }
		.ok_or_else(|| "C function call args were not a tuple".to_owned())
}

/// Binds a C method carrier to its receiver: instance access clones the
/// carrier with `m_self` filled; class access and METH_STATIC return the
/// carrier unbound; METH_CLASS binds the owning type.
unsafe extern "C" fn cfunction_descr_get(
	descriptor: *mut PyObject,
	instance: *mut PyObject,
	owner: *mut PyObject,
) -> *mut PyObject {
	if descriptor.is_null() {
		return abi::return_null_with_error("NULL C function descriptor");
	}
	// SAFETY: the descriptor protocol dispatches here only for live
	// PyCFunctionObject values (C_FUNCTION_TYPE's tp_descr_get).
	let function = unsafe { &*descriptor.cast::<PyCFunctionObject>() };
	let Some(method_def) = (unsafe { function.m_ml.as_ref() }) else {
		return abi::return_null_with_error("C function descriptor has no method definition");
	};
	let flags = method_def.ml_flags;
	if flags & METH_STATIC != 0 {
		return descriptor;
	}
	if flags & METH_CLASS != 0 {
		let receiver = if owner.is_null() { instance } else { owner };
		return alloc_cfunction_from_method_def_named(function.m_ml, receiver, function.name);
	}
	if instance.is_null() {
		return descriptor;
	}
	alloc_cfunction_from_method_def_named(function.m_ml, instance, function.name)
}

#[allow(dead_code)]
pub(super) fn alloc_cfunction(
	function: PyCFunction,
	flags: c_int,
	self_object: *mut PyObject,
	name: &str,
) -> *mut PyObject {
	let method_def = match synthesize_cfunction_method_def(function, flags, name) {
		Ok(method_def) => method_def,
		Err(message) => return abi::return_null_with_error(message),
	};
	alloc_cfunction_from_method_def_named(method_def, self_object, intern(name))
}

pub(super) fn alloc_cfunction_from_method_def(
	method_def: *mut PyMethodDef,
	self_object: *mut PyObject,
	name: &str,
) -> *mut PyObject {
	alloc_cfunction_from_method_def_named(method_def, self_object, intern(name))
}

fn alloc_cfunction_from_method_def_named(
	method_def: *mut PyMethodDef,
	self_object: *mut PyObject,
	name: u32,
) -> *mut PyObject {
	let Some(method_def_ref) = (unsafe { method_def.as_ref() }) else {
		return abi::return_null_with_error("C function method definition is NULL");
	};
	if method_def_ref.ml_meth.is_none() {
		return abi::return_null_with_error("C function method definition has no function pointer");
	}
	let info = pon_gc::GcTypeInfo {
		size:     mem::size_of::<PyCFunctionObject>(),
		trace:    trace_cfunction,
		finalize: None,
	};
	let Ok(block) = abi::alloc_gc_object(TYPE_ID_CAPI_CFUNCTION, info) else {
		return abi::return_null_with_error("runtime is not initialized");
	};
	let object = block.cast::<PyCFunctionObject>();
	// SAFETY: `block` is a fresh zeroed allocation of the carrier's size.
	unsafe {
		object.write(PyCFunctionObject {
			ob_base: PyObjectHeader::new(*C_FUNCTION_TYPE as *const PyType),
			m_ml: method_def,
			m_self: self_object,
			m_module: ptr::null_mut(),
			m_weakreflist: ptr::null_mut(),
			vectorcall: ptr::null_mut(),
			name,
		});
	}
	as_object_ptr(object)
}

/// Builds a process-lifetime method definition for Pon-created C-function
/// carriers that do not originate from an extension-owned `PyMethodDef`.
/// CPython-source extensions read `PyCFunctionObject.m_ml` directly, so the
/// leaked CString and `PyMethodDef` are intentional C-face ABI storage.
#[allow(dead_code)]
fn synthesize_cfunction_method_def(
	function: PyCFunction,
	flags: c_int,
	name: &str,
) -> Result<*mut PyMethodDef, String> {
	let c_name =
		CString::new(name).map_err(|_| "C function name contains an interior NUL".to_owned())?;
	let ml_name = c_name.into_raw();
	Ok(Box::into_raw(Box::new(PyMethodDef {
		ml_name,
		ml_meth: Some(function),
		ml_flags: flags,
		ml_doc: ptr::null(),
	})))
}

pub(super) fn c_string(ptr: *const c_char) -> Option<String> {
	if ptr.is_null() {
		return None;
	}
	Some(
		unsafe { CStr::from_ptr(ptr) }
			.to_string_lossy()
			.into_owned(),
	)
}

#[cfg(test)]
mod tests {
	use std::{
		env,
		ffi::OsStr,
		fs,
		path::{Path, PathBuf},
		process::{self, Command, Output},
		ptr,
		sync::atomic::{AtomicUsize, Ordering},
	};

	use super::load_extension_module;
	use crate::{
		abi::{format_object_for_print, pon_call, pon_const_int, pon_none, pon_runtime_init},
		import::{module_attr, reset_import_state_for_tests},
		intern::intern,
		thread_state::{pon_err_clear, pon_err_message, test_state_lock},
	};

	static NEXT_TEMP_ID: AtomicUsize = AtomicUsize::new(0);

	pub(super) struct TempExtensionRoot {
		path: PathBuf,
	}

	impl TempExtensionRoot {
		pub(super) fn new() -> Self {
			let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
			let path = env::temp_dir().join(format!("pon-capi-extension-{}-{id}", process::id()));
			let _ = fs::remove_dir_all(&path);
			fs::create_dir_all(&path).expect("create temporary C-extension root");
			Self { path }
		}

		pub(super) fn path(&self) -> &Path {
			&self.path
		}
	}

	impl Drop for TempExtensionRoot {
		fn drop(&mut self) {
			let _ = fs::remove_dir_all(&self.path);
		}
	}

	pub(super) struct ResetImportStateOnDrop;

	impl Drop for ResetImportStateOnDrop {
		fn drop(&mut self) {
			reset_import_state_for_tests();
		}
	}

	pub(super) fn compile_extension(
		temp: &TempExtensionRoot,
		module_name: &str,
		source: &str,
	) -> PathBuf {
		let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
		let source_path = temp.path().join(format!("{module_name}.c"));
		let output_path = temp.path().join(format!("{module_name}.pon.so"));
		fs::write(&source_path, source).expect("write temporary C extension source");

		let include_path = manifest.join("include");
		let bootstrap_path = manifest.join("capi").join("pon_capi_bootstrap.c");
		let args_path = manifest.join("capi").join("pon_capi_args.c");
		let mut args = vec![
			OsStr::new("-fPIC").to_owned(),
			OsStr::new("-DPON_CAPI_TESTING").to_owned(),
			OsStr::new("-I").to_owned(),
			include_path.as_os_str().to_owned(),
		];
		if cfg!(target_os = "macos") {
			args.push(OsStr::new("-dynamiclib").to_owned());
			args.push(OsStr::new("-undefined").to_owned());
			args.push(OsStr::new("dynamic_lookup").to_owned());
		} else {
			args.push(OsStr::new("-shared").to_owned());
		}
		args.push(source_path.as_os_str().to_owned());
		args.push(bootstrap_path.as_os_str().to_owned());
		args.push(args_path.as_os_str().to_owned());
		args.push(OsStr::new("-o").to_owned());
		args.push(output_path.as_os_str().to_owned());

		match run_compiler("cc", &args).or_else(|cc_error| {
			run_compiler("clang", &args)
				.map_err(|clang_error| format!("{cc_error}\n\nclang fallback:\n{clang_error}"))
		}) {
			Ok(()) => output_path,
			Err(message) => panic!("{message}"),
		}
	}

	fn run_compiler(compiler: &str, args: &[std::ffi::OsString]) -> Result<(), String> {
		let output = Command::new(compiler)
			.args(args)
			.output()
			.map_err(|error| format!("failed to run {compiler}: {error}"))?;
		if output.status.success() {
			Ok(())
		} else {
			Err(format_compiler_failure(compiler, &output))
		}
	}

	fn format_compiler_failure(compiler: &str, output: &Output) -> String {
		format!(
			"{compiler} failed with status {}\nstdout:\n{}\nstderr:\n{}",
			output.status,
			String::from_utf8_lossy(&output.stdout),
			String::from_utf8_lossy(&output.stderr)
		)
	}

	#[test]
	fn capi_loads_recompiled_extension_and_calls_exported_methods() {
		let _guard = test_state_lock();
		let _reset = ResetImportStateOnDrop;
		unsafe {
			assert_eq!(pon_runtime_init(), 0);
		}

		let temp = TempExtensionRoot::new();
		let module_path = compile_extension(
			&temp,
			"capi_test_ext",
			r#"
#include <Python.h>

static PyObject *answer(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    return PyLong_FromLong(42);
}

static PyObject *none_roundtrip(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    Py_INCREF(Py_None);
    Py_DECREF(Py_None);
    Py_RETURN_NONE;
}

static PyObject *echo(PyObject *self, PyObject *arg) {
    (void)self;
    Py_INCREF(arg);
    Py_DECREF(arg);
    Py_INCREF(arg);
    return arg;
}

static PyMethodDef methods[] = {
    {"answer", answer, METH_NOARGS, "return the answer"},
    {"none_roundtrip", none_roundtrip, METH_NOARGS, "exercise Py_None refs"},
    {"echo", echo, METH_O, "echo one object"},
    {NULL, NULL, 0, NULL}
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "capi_test_ext",
    "Pon C-API test extension",
    -1,
    methods
};

PyMODINIT_FUNC PyInit_capi_test_ext(void) {
    PyObject *m = PyModule_Create(&module);
    if (m == NULL) {
        return NULL;
    }
    if (PyModule_AddObject(m, "meaning", PyLong_FromLong(7)) < 0) {
        return NULL;
    }
    return m;
}
"#,
		);

		let module = load_extension_module("capi_test_ext", &module_path)
			.unwrap_or_else(|message| panic!("failed to load C extension: {message}"));
		assert!(!module.is_null(), "extension loader returned NULL module");

		let module_name = intern("capi_test_ext");
		let answer = module_attr(module_name, intern("answer")).expect("answer method registered");
		let result = unsafe { pon_call(answer, ptr::null_mut(), 0) };
		assert!(!result.is_null(), "answer() returned NULL: {:?}", pon_err_message());
		assert_eq!(format_object_for_print(result).as_deref(), Ok("42"));

		let meaning =
			module_attr(module_name, intern("meaning")).expect("module constant registered");
		assert_eq!(format_object_for_print(meaning).as_deref(), Ok("7"));

		let none_roundtrip = module_attr(module_name, intern("none_roundtrip"))
			.expect("none_roundtrip method registered");
		let none_result = unsafe { pon_call(none_roundtrip, ptr::null_mut(), 0) };
		assert_eq!(none_result, unsafe { pon_none() });

		let echo = module_attr(module_name, intern("echo")).expect("echo method registered");
		let argument = unsafe { pon_const_int(99) };
		let mut argv = [argument];
		let echoed = unsafe { pon_call(echo, argv.as_mut_ptr(), argv.len()) };
		assert!(!echoed.is_null(), "echo(99) returned NULL: {:?}", pon_err_message());
		assert_eq!(format_object_for_print(echoed).as_deref(), Ok("99"));
	}
	#[test]
	fn capi_multiphase_module_def_init_executes_slots_and_skips_create() {
		let _guard = test_state_lock();
		let _reset = ResetImportStateOnDrop;
		unsafe {
			assert_eq!(pon_runtime_init(), 0);
		}

		let temp = TempExtensionRoot::new();
		let module_path = compile_extension(
			&temp,
			"capi_multiphase_ext",
			r#"
#include <Python.h>

static long probe_mask = 0;

static PyObject *probe(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    probe_mask |= 1L << 2;
    return PyLong_FromLong(probe_mask);
}

static int multiphase_exec(PyObject *m) {
    long *state = (long *)PyModule_GetState(m);
    if (state == NULL) {
        return -1;
    }
    if (*state == 0) {
        probe_mask |= 1L << 0;
    }
    *state = 99;
    if (PyModule_AddObject(m, "token", PyLong_FromLong(42)) < 0) {
        return -1;
    }
    probe_mask |= 1L << 1;
    return 0;
}

static PyMethodDef methods[] = {
    {"probe", probe, METH_NOARGS, 0},
    {0, 0, 0, 0},
};

static PyModuleDef_Slot slots[] = {
    {Py_mod_exec, multiphase_exec},
    {Py_mod_multiple_interpreters, Py_MOD_MULTIPLE_INTERPRETERS_NOT_SUPPORTED},
    {Py_mod_gil, Py_MOD_GIL_NOT_USED},
    {0, 0},
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "capi_multiphase_ext",
    "multi-phase fixture",
    sizeof(long),
    methods,
    slots,
    0,
    0,
    0,
};

PyMODINIT_FUNC PyInit_capi_multiphase_ext(void) {
    return PyModuleDef_Init(&module);
}
"#,
		);

		let module = load_extension_module("capi_multiphase_ext", &module_path)
			.unwrap_or_else(|message| panic!("failed to load multi-phase C extension: {message}"));
		assert!(!module.is_null(), "multi-phase loader returned NULL module");

		let module_name = intern("capi_multiphase_ext");
		let token = module_attr(module_name, intern("token")).expect("Py_mod_exec added token");
		assert_eq!(format_object_for_print(token).as_deref(), Ok("42"));

		let probe = module_attr(module_name, intern("probe")).expect("probe method registered");
		let result = unsafe { pon_call(probe, ptr::null_mut(), 0) };
		assert!(!result.is_null(), "probe() returned NULL: {:?}", pon_err_message());
		assert_eq!(
			format_object_for_print(result).as_deref(),
			Ok("7"),
			"multi-phase probe bitmask mismatch"
		);

		let create_path = compile_extension(
			&temp,
			"capi_multiphase_create_ext",
			r#"
#include <Python.h>

static PyObject *make_module(PyObject *spec, PyModuleDef *def) {
    (void)spec;
    (void)def;
    Py_RETURN_NONE;
}

static PyModuleDef_Slot slots[] = {
    {Py_mod_create, make_module},
    {0, 0},
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "capi_multiphase_create_ext",
    0,
    -1,
    0,
    slots,
    0,
    0,
    0,
};

PyMODINIT_FUNC PyInit_capi_multiphase_create_ext(void) {
    return PyModuleDef_Init(&module);
}
"#,
		);
		let module = load_extension_module("capi_multiphase_create_ext", &create_path)
			.expect("Py_mod_create modules load with the default-created module");
		assert!(!module.is_null());
	}

	#[test]
	fn capi_type_and_error_identity_holds_across_the_boundary() {
		let _guard = test_state_lock();
		let _reset = ResetImportStateOnDrop;
		unsafe {
			assert_eq!(pon_runtime_init(), 0);
		}

		let temp = TempExtensionRoot::new();
		let module_path = compile_extension(
			&temp,
			"capi_twin_ext",
			r#"
#include <Python.h>

/* Returns a bitmask of passed checks; Rust asserts the full mask. */
static PyObject *identity_checks(PyObject *self, PyObject *args) {
    long ok = 0;
    (void)self;
    (void)args;

    PyObject *seven = PyLong_FromLong(7);
    if (Py_TYPE(seven) == &PyLong_Type) ok |= 1L << 0;
    if (Py_TYPE(seven) == Py_TYPE(seven)) ok |= 1L << 1;
    if (Py_TYPE(Py_None) == &_PyNone_Type) ok |= 1L << 2;
    if (Py_TYPE(Py_True) == &PyBool_Type) ok |= 1L << 3;
    if (PyLong_Type.tp_name != 0 && strcmp(PyLong_Type.tp_name, "int") == 0) ok |= 1L << 4;
    if (PyLong_Type.tp_basicsize > 0) ok |= 1L << 5;

    PyErr_SetString(PyExc_ValueError, "twin identity probe");
    if (PyErr_Occurred() == PyExc_ValueError) ok |= 1L << 6;
    if (((PyTypeObject *)PyExc_ValueError)->tp_flags & Py_TPFLAGS_BASE_EXC_SUBCLASS) ok |= 1L << 7;
    PyErr_Clear();
    if (PyErr_Occurred() == 0) ok |= 1L << 8;

    return PyLong_FromLong(ok);
}

static PyMethodDef twin_methods[] = {
    {"identity_checks", identity_checks, METH_NOARGS, 0},
    {0, 0, 0, 0},
};

static struct PyModuleDef twin_module = {
    PyModuleDef_HEAD_INIT,
    "capi_twin_ext",
    0,
    -1,
    twin_methods,
    0,
    0,
    0,
    0,
};

PyMODINIT_FUNC PyInit_capi_twin_ext(void) {
    return PyModule_Create(&twin_module);
}
"#,
		);

		let module = load_extension_module("capi_twin_ext", &module_path)
			.unwrap_or_else(|message| panic!("failed to load C extension: {message}"));
		assert!(!module.is_null(), "extension loader returned NULL module");

		let module_name = intern("capi_twin_ext");
		let checks =
			module_attr(module_name, intern("identity_checks")).expect("identity_checks registered");
		let result = unsafe { pon_call(checks, ptr::null_mut(), 0) };
		assert!(!result.is_null(), "identity_checks() returned NULL: {:?}", pon_err_message());
		// All nine identity bits must hold; a partial mask names the failure.
		assert_eq!(
			format_object_for_print(result).as_deref(),
			Ok("511"),
			"twin identity bitmask mismatch"
		);
	}
}
