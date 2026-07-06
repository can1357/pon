//! Runtime PEP 695 objects: `TypeAliasType`, `TypeVar`, and `GenericAlias`.
//!
//! `type X = expr` lowers to a zero-argument value thunk plus
//! [`pon_make_type_alias`]; the alias evaluates `expr` lazily on first
//! `__value__` access and caches the result (CPython 3.14 semantics).
//! `def f[T](...)` binds `T` through [`pon_make_typevar`] inside synthesized
//! annotate/alias scopes.  `GenericAlias` carries `origin[args]` subscript
//! results (`list[int]`) produced by the builtin-constructor subscript
//! fallback in `abstract_op::subscript_get`.

use core::{
	ffi::c_int,
	mem::{offset_of, size_of},
	ptr,
};
use std::{
	collections::{HashMap, HashSet},
	sync::LazyLock,
};

use crate::{
	intern::resolve,
	object::{
		PyFunction, PyMappingMethods, PyNumberMethods, PyObject, PyObjectHeader, PyType, PyUnicode,
		as_object_ptr,
	},
	thread_state::pon_err_set,
};

/// Runtime object for Python 3.12+ `type X = ...` aliases.
///
/// The evaluated value is computed lazily: `thunk` is a zero-argument
/// synthesized function evaluating the alias body, and `value` caches its
/// first result.  This mirrors CPython's `TypeAliasType.__value__` laziness
/// (forward references in the alias body resolve at access time, not at the
/// `type` statement).
#[repr(C)]
#[derive(Debug)]
pub struct PyTypeAlias {
	/// Common object header; this field must remain first.
	pub ob_base:       PyObjectHeader,
	/// Interned alias name.
	pub name_interned: u32,
	/// Zero-argument value thunk, or NULL for eagerly-built aliases.
	pub thunk:         *mut PyObject,
	/// Cached evaluated value, or NULL until first `__value__` access.
	pub value:         *mut PyObject,
}

impl PyTypeAlias {
	/// Builds a type-alias payload for an allocated object slot.
	#[must_use]
	pub const fn new(ty: *const PyType, name_interned: u32, thunk: *mut PyObject) -> Self {
		Self { ob_base: PyObjectHeader::new(ty), name_interned, thunk, value: ptr::null_mut() }
	}
}

/// Minimal PEP 695 `TypeVar`: an interned name with CPython's bare-name repr.
#[repr(C)]
#[derive(Debug)]
pub struct PyTypeVar {
	/// Common object header; this field must remain first.
	pub ob_base:       PyObjectHeader,
	/// Interned type-parameter name (`T`).
	pub name_interned: u32,
}

/// Minimal `types.GenericAlias`: `origin[args]` (`list[int]`).
///
/// The payload fields are Rust-only (never read through the C ABI), so the
/// `Vec` behind `repr(C)` is acceptable; only `ob_base` has a layout contract.
#[repr(C)]
#[derive(Debug)]
pub struct PyGenericAlias {
	/// Common object header; this field must remain first.
	pub ob_base: PyObjectHeader,
	/// Subscripted constructor (`list` in `list[int]`).
	pub origin:  *mut PyObject,
	/// Subscript arguments, tuple-flattened (`[str, int]` in `dict[str, int]`).
	pub args:    Vec<*mut PyObject>,
}

/// Minimal `types.UnionType` payload for `A | B` type expressions.
#[repr(C)]
#[derive(Debug)]
pub struct PyUnionType {
	/// Common object header; must remain first.
	pub ob_base: PyObjectHeader,
	/// Union members in source order.
	pub args:    Vec<*mut PyObject>,
}

fn resolved_name(name_interned: u32) -> String {
	resolve(name_interned).unwrap_or_else(|| format!("<interned:{name_interned}>"))
}

unsafe fn attribute_name(name: *mut PyObject) -> Option<&'static str> {
	if name.is_null() {
		return None;
	}
	unsafe { (&*name.cast::<PyUnicode>()).as_str() }
}

fn raise_attr(message: String) -> *mut PyObject {
	pon_err_set(message);
	ptr::null_mut()
}

/// Returns the process-lifetime `TypeAliasType` descriptor.
///
/// Named `typing.TypeAliasType` so `print(type(X))` matches CPython
/// (`<class 'typing.TypeAliasType'>`).
#[must_use]
pub fn type_alias_type(type_type: *const PyType) -> *mut PyType {
	let _ = type_type;
	static TYPE: LazyLock<usize> = LazyLock::new(|| {
		let mut ty = PyType::new(core::ptr::null(), "typing.TypeAliasType", size_of::<PyTypeAlias>());
		ty.tp_getattro = Some(type_alias_getattro);
		ty.tp_as_mapping = type_alias_mapping_methods();
		Box::into_raw(Box::new(ty)) as usize
	});
	*TYPE as *mut PyType
}

/// Returns the process-lifetime `TypeVar` descriptor.
#[must_use]
pub fn typevar_type() -> *mut PyType {
	static TYPE: LazyLock<usize> = LazyLock::new(|| {
		let mut ty = PyType::new(core::ptr::null(), "TypeVar", size_of::<PyTypeVar>());
		ty.tp_getattro = Some(typevar_getattro);
		Box::into_raw(Box::new(ty)) as usize
	});
	*TYPE as *mut PyType
}

/// Returns the process-lifetime `types.GenericAlias` descriptor.
#[must_use]
pub fn generic_alias_type() -> *mut PyType {
	static TYPE: LazyLock<usize> = LazyLock::new(|| {
		let mut ty =
			PyType::new(core::ptr::null(), "types.GenericAlias", size_of::<PyGenericAlias>());
		ty.tp_getattro = Some(generic_alias_getattro);
		ty.tp_new = Some(generic_alias_new);
		Box::into_raw(Box::new(ty)) as usize
	});
	*TYPE as *mut PyType
}

/// Returns the process-lifetime `types.UnionType` descriptor.
#[must_use]
pub fn union_type() -> *mut PyType {
	static TYPE: LazyLock<usize> = LazyLock::new(|| {
		let mut ty = PyType::new(core::ptr::null(), "types.UnionType", size_of::<PyUnionType>());
		ty.tp_getattro = Some(union_getattro);
		ty.tp_hash = Some(union_hash_slot);
		ty.tp_richcmp = Some(union_richcmp);
		install_union_or_slots(&mut ty);
		Box::into_raw(Box::new(ty)) as usize
	});
	*TYPE as *mut PyType
}

/// True when `object` is a boxed `PyTypeAlias`.
#[must_use]
pub fn is_type_alias(object: *mut PyObject) -> bool {
	!object.is_null() && unsafe { (*object).ob_type } == type_alias_type(ptr::null()).cast_const()
}

/// True when `object` is a boxed `PyTypeVar`.
#[must_use]
pub fn is_typevar(object: *mut PyObject) -> bool {
	!object.is_null() && unsafe { (*object).ob_type } == typevar_type().cast_const()
}

/// True when `object` is a boxed `PyGenericAlias`.
#[must_use]
pub fn is_generic_alias(object: *mut PyObject) -> bool {
	!object.is_null() && unsafe { (*object).ob_type } == generic_alias_type().cast_const()
}

/// Allocates a boxed `TypeAliasType` with a lazy value thunk.
///
/// The object is leaked intentionally: aliases are module-lifetime objects and
/// the runtime has no registered GC family for them (same accepted pattern as
/// the function metadata side tables).
#[must_use]
pub fn new_type_alias(
	name_interned: u32,
	thunk: *mut PyObject,
	type_type: *const PyType,
) -> *mut PyObject {
	let ty = type_alias_type(type_type);
	as_object_ptr(Box::into_raw(Box::new(PyTypeAlias::new(ty.cast_const(), name_interned, thunk))))
}

/// Allocates a boxed minimal `TypeVar`.
#[must_use]
pub fn new_typevar(name_interned: u32) -> *mut PyObject {
	let object = Box::new(PyTypeVar {
		ob_base: PyObjectHeader::new(typevar_type().cast_const()),
		name_interned,
	});
	as_object_ptr(Box::into_raw(object))
}

/// Allocates a boxed `GenericAlias` for `origin[args]`.
#[must_use]
pub fn new_generic_alias(origin: *mut PyObject, args: Vec<*mut PyObject>) -> *mut PyObject {
	let object = Box::new(PyGenericAlias {
		ob_base: PyObjectHeader::new(generic_alias_type().cast_const()),
		origin,
		args,
	});
	as_object_ptr(Box::into_raw(object))
}

unsafe extern "C" fn generic_alias_new(
	_subtype: *mut PyType,
	args: *mut PyObject,
	kwargs: *mut PyObject,
) -> *mut PyObject {
	if !kwargs.is_null() {
		return crate::abi::return_null_with_type_error(
			"types.GenericAlias does not accept keyword arguments",
		);
	}
	make_generic_alias_from_args(args)
}

pub unsafe extern "C" fn pon_make_generic_alias(
	origin: *mut PyObject,
	args: *mut PyObject,
) -> *mut PyObject {
	crate::untag_prelude!(origin, args);
	if origin.is_null() {
		return crate::abi::return_null_with_type_error("types.GenericAlias origin must not be NULL");
	}
	let alias_args = match unsafe { crate::abi::seq::exact_tuple_slice(args) } {
		Some(entries) => entries.to_vec(),
		None => vec![args],
	};
	new_generic_alias(origin, alias_args)
}

fn make_generic_alias_from_args(args: *mut PyObject) -> *mut PyObject {
	let Some(items) = (unsafe { crate::abi::seq::exact_tuple_slice(args) }) else {
		return crate::abi::return_null_with_type_error("types.GenericAlias args must be a tuple");
	};
	if items.len() != 2 {
		return crate::abi::return_null_with_type_error(
			"types.GenericAlias expects exactly 2 arguments",
		);
	}
	unsafe { pon_make_generic_alias(items[0], items[1]) }
}

/// Allocates a boxed normalized `UnionType` for `left | right`.
#[must_use]
pub fn new_union_type(args: Vec<*mut PyObject>) -> *mut PyObject {
	let object =
		Box::new(PyUnionType { ob_base: PyObjectHeader::new(union_type().cast_const()), args });
	as_object_ptr(Box::into_raw(object))
}

/// Installs PEP 604 `type.__or__`/`type.__ror__` slots on the metatype.
pub unsafe fn install_type_or_slots(ty: *mut PyType) {
	if let Some(ty) = unsafe { ty.as_mut() } {
		install_union_or_slots(ty);
	}
}

fn type_alias_mapping_methods() -> *mut PyMappingMethods {
	static METHODS: LazyLock<usize> = LazyLock::new(|| {
		let mut methods = PyMappingMethods::EMPTY;
		methods.mp_subscript = Some(type_alias_subscript);
		Box::into_raw(Box::new(methods)) as usize
	});
	*METHODS as *mut PyMappingMethods
}

unsafe extern "C" fn type_alias_subscript(
	object: *mut PyObject,
	key: *mut PyObject,
) -> *mut PyObject {
	let value = unsafe { type_alias_value(object) };
	if value.is_null() {
		return ptr::null_mut();
	}
	let params = collect_type_params(value);
	if params.is_empty() {
		return unsafe { crate::abstract_op::subscript_get(value, key) };
	}
	let args = match unsafe { crate::abi::seq::exact_tuple_slice(key) } {
		Some(entries) => entries.to_vec(),
		None => vec![key],
	};
	if args.len() != params.len() {
		return crate::abi::return_null_with_type_error(format!(
			"typing.TypeAliasType expected {} type argument(s), got {}",
			params.len(),
			args.len()
		));
	}
	let bindings = params
		.into_iter()
		.zip(args)
		.map(|(param, arg)| (param as usize, arg))
		.collect::<HashMap<usize, *mut PyObject>>();
	substitute_type_params(value, &bindings)
}

fn collect_type_params(value: *mut PyObject) -> Vec<*mut PyObject> {
	let mut seen = HashSet::new();
	let mut params = Vec::new();
	collect_type_params_inner(value, &mut seen, &mut params);
	params
}

fn collect_type_params_inner(
	value: *mut PyObject,
	seen: &mut HashSet<usize>,
	out: &mut Vec<*mut PyObject>,
) {
	if is_typevar(value) {
		if seen.insert(value as usize) {
			out.push(value);
		}
		return;
	}
	if is_generic_alias(value) {
		let alias = unsafe { &*value.cast::<PyGenericAlias>() };
		for &arg in &alias.args {
			collect_type_params_inner(arg, seen, out);
		}
		return;
	}
	if is_union_type(value) {
		for &arg in union_args(value) {
			collect_type_params_inner(arg, seen, out);
		}
	}
}

fn substitute_type_params(
	value: *mut PyObject,
	bindings: &HashMap<usize, *mut PyObject>,
) -> *mut PyObject {
	if let Some(bound) = bindings.get(&(value as usize)).copied() {
		return bound;
	}
	if is_generic_alias(value) {
		let alias = unsafe { &*value.cast::<PyGenericAlias>() };
		let args = alias
			.args
			.iter()
			.copied()
			.map(|arg| substitute_type_params(arg, bindings))
			.collect();
		return new_generic_alias(alias.origin, args);
	}
	if is_union_type(value) {
		let args = union_args(value)
			.iter()
			.copied()
			.map(|arg| substitute_type_params(arg, bindings))
			.collect();
		return new_union_type(args);
	}
	value
}

fn install_union_or_slots(ty: &mut PyType) {
	if ty.tp_as_number.is_null() {
		ty.tp_as_number = union_number_methods();
	} else if let Some(methods) = unsafe { ty.tp_as_number.as_mut() } {
		methods.nb_or = Some(union_or_slot);
		methods.nb_reflected_or = Some(union_or_slot);
	}
}

fn union_number_methods() -> *mut PyNumberMethods {
	static METHODS: LazyLock<usize> = LazyLock::new(|| {
		let mut methods = PyNumberMethods::EMPTY;
		methods.nb_or = Some(union_or_slot);
		methods.nb_reflected_or = Some(union_or_slot);
		Box::into_raw(Box::new(methods)) as usize
	});
	*METHODS as *mut PyNumberMethods
}

unsafe extern "C" fn union_or_slot(left: *mut PyObject, right: *mut PyObject) -> *mut PyObject {
	match union_from_operands(left, right) {
		Some(value) => value,
		None => unsafe { crate::abi::pon_not_implemented() },
	}
}

fn union_from_operands(left: *mut PyObject, right: *mut PyObject) -> Option<*mut PyObject> {
	let mut args = Vec::new();
	if !collect_union_args(&mut args, left) || !collect_union_args(&mut args, right) {
		return None;
	}
	if args.len() == 1 {
		return args.first().copied();
	}
	Some(new_union_type(args))
}

fn collect_union_args(out: &mut Vec<*mut PyObject>, object: *mut PyObject) -> bool {
	if object.is_null() {
		return false;
	}
	if is_union_type(object) {
		for arg in union_args(object) {
			push_unique_arg(out, *arg);
		}
		return true;
	}
	let Some(arg) = normalized_union_arg(object) else {
		return false;
	};
	push_unique_arg(out, arg);
	true
}

fn push_unique_arg(out: &mut Vec<*mut PyObject>, arg: *mut PyObject) {
	if !out.contains(&arg) {
		out.push(arg);
	}
}

fn normalized_union_arg(object: *mut PyObject) -> Option<*mut PyObject> {
	if is_none_object(object) {
		return unsafe { Some((*object).ob_type.cast_mut().cast::<PyObject>()) };
	}
	if is_type_object(object)
		|| is_generic_alias(object)
		|| is_type_alias(object)
		|| is_typevar(object)
	{
		Some(object)
	} else {
		None
	}
}

fn is_none_object(object: *mut PyObject) -> bool {
	if object.is_null() {
		return false;
	}
	let ty = unsafe { (*object).ob_type };
	!ty.is_null() && unsafe { (*ty).name() == "NoneType" }
}

fn is_type_object(object: *mut PyObject) -> bool {
	if object.is_null() {
		return false;
	}
	let meta = unsafe { (*object).ob_type.cast_mut() };
	!meta.is_null() && unsafe { crate::mro::is_subtype(meta, crate::abi::runtime_type_type()) }
}

/// True when `object` is a boxed `UnionType`.
#[must_use]
pub fn is_union_type(object: *mut PyObject) -> bool {
	!object.is_null() && unsafe { (*object).ob_type } == union_type().cast_const()
}

/// Borrow normalized union arguments.
#[must_use]
pub fn union_args(object: *mut PyObject) -> &'static [*mut PyObject] {
	if !is_union_type(object) {
		&[]
	} else {
		unsafe { &(*object.cast::<PyUnionType>()).args }
	}
}

/// Repr for PEP 604 unions, e.g. `int | str` and `int | None`.
#[must_use]
pub fn union_repr(object: *mut PyObject) -> String {
	union_args(object)
		.iter()
		.copied()
		.map(union_arg_text)
		.collect::<Vec<_>>()
		.join(" | ")
}

fn union_arg_text(arg: *mut PyObject) -> String {
	if is_none_type_object(arg) {
		"None".to_owned()
	} else {
		generic_arg_text(arg)
	}
}

fn is_none_type_object(object: *mut PyObject) -> bool {
	is_type_object(object) && unsafe { (*object.cast::<PyType>()).name() == "NoneType" }
}

/// Order-insensitive equality for normalized union payloads.
#[must_use]
pub fn union_equal(left: *mut PyObject, right: *mut PyObject) -> bool {
	let left = union_args(left);
	let right = union_args(right);
	left.len() == right.len() && left.iter().all(|arg| right.contains(arg))
}

/// Order-insensitive stable hash for `hash(types.UnionType)`.
#[must_use]
pub fn union_hash(object: *mut PyObject) -> isize {
	let mut acc = 0x3456_789a_bcde_f012_u64 ^ (union_args(object).len() as u64);
	for arg in union_args(object) {
		let lane = union_arg_hash(*arg) as u64;
		acc ^= lane.wrapping_add(0x9e37_79b9_7f4a_7c15).rotate_left(13);
	}
	let hash = acc as isize;
	if hash == -1 { -2 } else { hash }
}

fn union_arg_hash(arg: *mut PyObject) -> isize {
	let text = union_arg_text(arg);
	let mut hash = 0xcbf2_9ce4_8422_2325_u64;
	for byte in text.as_bytes() {
		hash ^= u64::from(*byte);
		hash = hash.wrapping_mul(0x100_0000_01b3);
	}
	let hash = hash as isize;
	if hash == -1 { -2 } else { hash }
}

unsafe extern "C" fn union_hash_slot(object: *mut PyObject) -> isize {
	union_hash(object)
}

unsafe extern "C" fn union_richcmp(
	left: *mut PyObject,
	right: *mut PyObject,
	op: c_int,
) -> *mut PyObject {
	let equal = is_union_type(right) && union_equal(left, right);
	match op {
		2 => unsafe { crate::abi::number::pon_const_bool(i32::from(equal)) },
		3 => unsafe { crate::abi::number::pon_const_bool(i32::from(!equal)) },
		_ => unsafe { crate::abi::pon_not_implemented() },
	}
}

/// C ABI constructor for `InstKind::MakeTypeAlias` (`type X = expr`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_make_type_alias(
	name_interned: u32,
	thunk: *mut PyObject,
) -> *mut PyObject {
	crate::untag_prelude!(thunk);
	if thunk.is_null() {
		pon_err_set("type alias thunk is NULL");
		return ptr::null_mut();
	}
	new_type_alias(name_interned, thunk, core::ptr::null())
}

/// C ABI constructor for `InstKind::MakeTypeVar` (`def f[T](...)`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_make_typevar(name_interned: u32) -> *mut PyObject {
	new_typevar(name_interned)
}

/// Lazily evaluates and caches the alias value (`X.__value__`).
pub unsafe fn type_alias_value(alias: *mut PyObject) -> *mut PyObject {
	let alias = alias.cast::<PyTypeAlias>();
	let cached = unsafe { (*alias).value };
	if !cached.is_null() {
		return cached;
	}
	let thunk = unsafe { (*alias).thunk };
	if thunk.is_null() {
		return raise_attr("type alias has no value thunk".to_owned());
	}
	let value = unsafe { crate::abi::pon_call(thunk, ptr::null_mut(), 0) };
	if value.is_null() {
		return ptr::null_mut();
	}
	unsafe {
		(*alias).value = value;
	}
	value
}

unsafe extern "C" fn type_alias_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let Some(name_text) = (unsafe { attribute_name(name) }) else {
		return raise_attr("type alias attribute name must be str".to_owned());
	};
	match name_text {
		"__name__" => {
			let text = resolved_name(unsafe { (*object.cast::<PyTypeAlias>()).name_interned });
			unsafe { crate::abi::pon_const_str(text.as_ptr(), text.len()) }
		},
		"__value__" => unsafe { type_alias_value(object) },
		"__type_params__" => type_alias_type_params(object),
		_ => raise_attr(format!("'typing.TypeAliasType' object has no attribute '{name_text}'")),
	}
}

fn type_alias_type_params(object: *mut PyObject) -> *mut PyObject {
	let value = unsafe { type_alias_value(object) };
	if value.is_null() {
		return ptr::null_mut();
	}
	let params = collect_type_params(value);
	unsafe {
		crate::abi::seq::pon_build_tuple(
			if params.is_empty() {
				ptr::null_mut()
			} else {
				params.as_ptr().cast_mut()
			},
			params.len(),
		)
	}
}

unsafe extern "C" fn typevar_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let Some(name_text) = (unsafe { attribute_name(name) }) else {
		return raise_attr("TypeVar attribute name must be str".to_owned());
	};
	match name_text {
		"__name__" => {
			let text = resolved_name(unsafe { (*object.cast::<PyTypeVar>()).name_interned });
			unsafe { crate::abi::pon_const_str(text.as_ptr(), text.len()) }
		},
		_ => raise_attr(format!("'TypeVar' object has no attribute '{name_text}'")),
	}
}

unsafe extern "C" fn generic_alias_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let Some(name_text) = (unsafe { attribute_name(name) }) else {
		return raise_attr("GenericAlias attribute name must be str".to_owned());
	};
	let alias = unsafe { &*object.cast::<PyGenericAlias>() };
	match name_text {
		"__origin__" => alias.origin,
		"__args__" => crate::native::builtins_mod::alloc_tuple(alias.args.clone()),
		_ => raise_attr(format!("'types.GenericAlias' object has no attribute '{name_text}'")),
	}
}

unsafe extern "C" fn union_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let Some(name_text) = (unsafe { attribute_name(name) }) else {
		return raise_attr("union attribute name must be str".to_owned());
	};
	match name_text {
		"__args__" => {
			let mut args = unsafe { (&*object.cast::<PyUnionType>()).args.clone() };
			unsafe {
				crate::abi::seq::pon_build_tuple(
					if args.is_empty() {
						ptr::null_mut()
					} else {
						args.as_mut_ptr()
					},
					args.len(),
				)
			}
		},
		_ => raise_attr(format!("'types.UnionType' object has no attribute '{name_text}'")),
	}
}

/// Bare alias name used by `repr(X)`/`print(X)` (CPython: `repr(X) == 'X'`).
#[must_use]
pub fn type_alias_repr(object: *mut PyObject) -> String {
	resolved_name(unsafe { (*object.cast::<PyTypeAlias>()).name_interned })
}

/// Bare parameter name used by `repr(T)` (CPython: `repr(T) == 'T'`).
#[must_use]
pub fn typevar_repr(object: *mut PyObject) -> String {
	resolved_name(unsafe { (*object.cast::<PyTypeVar>()).name_interned })
}

/// `origin[arg, ...]` repr matching CPython's `types.GenericAlias`
/// (`repr(list[int]) == 'list[int]'`: type-ish args render as bare names).
#[must_use]
pub fn generic_alias_repr(object: *mut PyObject) -> String {
	let alias = unsafe { &*object.cast::<PyGenericAlias>() };
	let args = alias
		.args
		.iter()
		.copied()
		.map(generic_arg_text)
		.collect::<Vec<_>>()
		.join(", ");
	format!("{}[{args}]", generic_arg_text(alias.origin))
}

/// Formats one subscript argument the way CPython prints generic parameters:
/// classes and constructor functions as bare names, everything else as repr.
fn generic_arg_text(arg: *mut PyObject) -> String {
	if arg.is_null() {
		return "<NULL>".to_owned();
	}
	if is_typevar(arg) {
		return typevar_repr(arg);
	}
	if is_type_alias(arg) {
		return type_alias_repr(arg);
	}
	if is_generic_alias(arg) {
		return generic_alias_repr(arg);
	}
	unsafe {
		let ty = (*arg).ob_type;
		if !ty.is_null() {
			let ty_name = (*ty).name();
			if ty_name == "type" {
				return (*arg.cast::<PyType>()).name().to_owned();
			}
			if ty_name == "function" {
				// pon builtin constructors (`int`, `str`, `list`, ...) are
				// native functions; render their bare name like a class.
				let function = &*arg.cast::<PyFunction>();
				return resolved_name(function.name_interned);
			}
		}
	}
	crate::native::builtins_mod::repr_text(arg)
}

/// Constructor names accepted by the builtin subscript fallback
/// (`list[int]`, `dict[str, int]`, ...).  pon builtins are `PyFunction`
/// objects, not `PyType`s, so plain `mp_subscript` dispatch never fires.
#[must_use]
pub fn is_subscriptable_builtin_constructor(name: &str) -> bool {
	matches!(
		name,
		"list"
			| "dict"
			| "tuple"
			| "set"
			| "frozenset"
			| "type"
			| "int"
			| "str"
			| "float"
			| "bool"
			| "bytes"
	)
}

const _: () = {
	assert!(offset_of!(PyTypeAlias, ob_base) == 0);
	assert!(offset_of!(PyTypeVar, ob_base) == 0);
	assert!(offset_of!(PyGenericAlias, ob_base) == 0);
};
