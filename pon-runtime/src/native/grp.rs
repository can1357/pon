//! Native `grp` module backed by the host group database.
//!
//! The module mirrors CPython's libc-backed lookup surface and returns
//! `struct_group` objects supporting named fields plus tuple-style indexing.

use std::{
	ffi::{CStr, CString},
	ptr,
	sync::LazyLock,
};

use num_traits::ToPrimitive;

use super::install_module;
use crate::{
	abi::{self, pon_const_int, pon_const_str, pon_make_function},
	intern::intern,
	object::{PyObject, PyObjectHeader, PySequenceMethods, PyType},
	types::{exc::ExceptionKind, type_::unicode_text},
};

const VARIADIC_ARITY: usize = crate::native::builtins_mod::VARIADIC_ARITY;
const GROUP_FIELDS: [&str; 4] = ["gr_name", "gr_passwd", "gr_gid", "gr_mem"];

#[derive(Clone, Debug)]
struct GroupRecord {
	name:    String,
	passwd:  String,
	gid:     i64,
	members: Vec<String>,
}

#[repr(C)]
struct PyGroup {
	ob_base: PyObjectHeader,
	record:  GroupRecord,
}

static GROUP_SEQUENCE: LazyLock<PySequenceMethods> = LazyLock::new(|| PySequenceMethods {
	sq_length: Some(group_len),
	sq_item: Some(group_item),
	..PySequenceMethods::EMPTY
});

static GROUP_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"grp.struct_group",
		std::mem::size_of::<PyGroup>(),
	);
	ty.tp_as_sequence = &*GROUP_SEQUENCE as *const PySequenceMethods as *mut PySequenceMethods;
	ty.tp_getattro = Some(group_getattro);
	ty.tp_repr = Some(group_repr);
	Box::into_raw(Box::new(ty)) as usize
});

fn group_type() -> *mut PyType {
	*GROUP_TYPE as *mut PyType
}

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	let name = "grp";
	let mut attrs = vec![string_attr("__name__", name)?];
	attrs.push(function_attr("getgrnam", grp_getgrnam)?);
	attrs.push(function_attr("getgrgid", grp_getgrgid)?);
	attrs.push(function_attr("getgrall", grp_getgrall)?);
	attrs.push((intern("struct_group"), group_type().cast::<PyObject>()));
	install_module(name, attrs)
}

fn string_attr(name: &str, value: &str) -> Result<(u32, *mut PyObject), String> {
	let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
	(!object.is_null())
		.then_some((intern(name), object))
		.ok_or_else(|| format!("failed to allocate grp.{name}"))
}

fn function_attr(
	name: &str,
	entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> Result<(u32, *mut PyObject), String> {
	let function = unsafe { pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
	(!function.is_null())
		.then_some((intern(name), function))
		.ok_or_else(|| format!("failed to allocate grp.{name}"))
}

unsafe extern "C" fn grp_getgrnam(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { arg_slice(argv, argc) } {
		Some(args) if args.len() == 1 => args,
		_ => return raise_type_error(&format!("getgrnam() takes exactly 1 argument ({argc} given)")),
	};
	let name = match string_arg(args[0], "getgrnam") {
		Ok(name) => name,
		Err(error) => return error,
	};
	let c_name = match CString::new(name.as_str()) {
		Ok(value) => value,
		Err(_) => return raise_value_error("embedded null character"),
	};
	let entry = unsafe { libc::getgrnam(c_name.as_ptr()) };
	if entry.is_null() {
		return raise_key_error(&format!("getgrnam(): name not found: {name}"));
	}
	match unsafe { record_from_group(entry) } {
		Ok(record) => group_object(record),
		Err(error) => error,
	}
}

unsafe extern "C" fn grp_getgrgid(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match unsafe { arg_slice(argv, argc) } {
		Some(args) if args.len() == 1 => args,
		_ => return raise_type_error(&format!("getgrgid() takes exactly 1 argument ({argc} given)")),
	};
	let gid = match gid_arg(args[0]) {
		Ok(gid) => gid,
		Err(error) => return error,
	};
	let entry = unsafe { libc::getgrgid(gid) };
	if entry.is_null() {
		return raise_key_error(&format!("getgrgid(): gid not found: {gid}"));
	}
	match unsafe { record_from_group(entry) } {
		Ok(record) => group_object(record),
		Err(error) => error,
	}
}

unsafe extern "C" fn grp_getgrall(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 0 || !argv.is_null() {
		return raise_type_error(&format!("getgrall() takes no arguments ({argc} given)"));
	}
	let mut items = Vec::new();
	unsafe { libc::setgrent() };
	loop {
		let entry = unsafe { libc::getgrent() };
		if entry.is_null() {
			break;
		}
		let record = match unsafe { record_from_group(entry) } {
			Ok(record) => record,
			Err(error) => {
				unsafe { libc::endgrent() };
				return error;
			},
		};
		let object = group_object(record);
		if object.is_null() {
			unsafe { libc::endgrent() };
			return ptr::null_mut();
		}
		items.push(object);
	}
	unsafe { libc::endgrent() };
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

unsafe fn record_from_group(entry: *mut libc::group) -> Result<GroupRecord, *mut PyObject> {
	if entry.is_null() {
		return Err(raise_key_error("group entry not found"));
	}
	let raw = unsafe { &*entry };
	Ok(GroupRecord {
		name:    c_string(raw.gr_name),
		passwd:  c_string(raw.gr_passwd),
		gid:     i64::from(raw.gr_gid),
		members: member_list(raw.gr_mem),
	})
}

fn c_string(ptr: *const libc::c_char) -> String {
	if ptr.is_null() {
		String::new()
	} else {
		unsafe { CStr::from_ptr(ptr) }
			.to_string_lossy()
			.into_owned()
	}
}

fn member_list(mut ptrs: *mut *mut libc::c_char) -> Vec<String> {
	let mut members = Vec::new();
	if ptrs.is_null() {
		return members;
	}
	unsafe {
		while !(*ptrs).is_null() {
			members.push(c_string(*ptrs));
			ptrs = ptrs.add(1);
		}
	}
	members
}

fn group_object(record: GroupRecord) -> *mut PyObject {
	Box::into_raw(Box::new(PyGroup { ob_base: PyObjectHeader::new(group_type()), record }))
		.cast::<PyObject>()
}

unsafe extern "C" fn group_len(_object: *mut PyObject) -> isize {
	GROUP_FIELDS.len() as isize
}

unsafe extern "C" fn group_item(object: *mut PyObject, index: isize) -> *mut PyObject {
	let Ok(index) = usize::try_from(index) else {
		return raise_index_error("tuple index out of range");
	};
	let record = unsafe { &(*object.cast::<PyGroup>()).record };
	group_field(record, index).unwrap_or_else(|| raise_index_error("tuple index out of range"))
}

unsafe extern "C" fn group_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let Some(name_text) = (unsafe { unicode_text(crate::tag::untag_arg(name)) }) else {
		return raise_type_error("attribute name must be str");
	};
	if name_text == "n_fields" || name_text == "n_sequence_fields" {
		return unsafe { pon_const_int(GROUP_FIELDS.len() as i64) };
	}
	if name_text == "n_unnamed_fields" {
		return unsafe { pon_const_int(0) };
	}
	let record = unsafe { &(*object.cast::<PyGroup>()).record };
	if let Some(index) = GROUP_FIELDS.iter().position(|&field| field == name_text) {
		return group_field(record, index).unwrap_or(ptr::null_mut());
	}
	unsafe { abi::exc::pon_raise_attribute_error(object, intern(name_text)) }
}

unsafe extern "C" fn group_repr(object: *mut PyObject) -> *mut PyObject {
	let record = unsafe { &(*object.cast::<PyGroup>()).record };
	let members = record
		.members
		.iter()
		.map(|member| member.repr_quote())
		.collect::<Vec<_>>()
		.join(", ");
	let text = format!(
		"grp.struct_group(gr_name={}, gr_passwd={}, gr_gid={}, gr_mem=[{}])",
		record.name.repr_quote(),
		record.passwd.repr_quote(),
		record.gid,
		members
	);
	unsafe { pon_const_str(text.as_ptr(), text.len()) }
}

trait ReprQuote {
	fn repr_quote(&self) -> String;
}

impl ReprQuote for str {
	fn repr_quote(&self) -> String {
		let mut out = String::from("'");
		for ch in self.chars() {
			match ch {
				'\\' => out.push_str("\\\\"),
				'\'' => out.push_str("\\'"),
				'\n' => out.push_str("\\n"),
				'\r' => out.push_str("\\r"),
				'\t' => out.push_str("\\t"),
				ch => out.push(ch),
			}
		}
		out.push('\'');
		out
	}
}

fn group_field(record: &GroupRecord, index: usize) -> Option<*mut PyObject> {
	match index {
		0 => Some(str_object(&record.name)),
		1 => Some(str_object(&record.passwd)),
		2 => Some(unsafe { pon_const_int(record.gid) }),
		3 => Some(member_list_object(&record.members)),
		_ => None,
	}
}

fn member_list_object(members: &[String]) -> *mut PyObject {
	let mut objects = Vec::with_capacity(members.len());
	for member in members {
		let object = str_object(member);
		if object.is_null() {
			return ptr::null_mut();
		}
		objects.push(object);
	}
	unsafe {
		abi::seq::pon_build_list(
			if objects.is_empty() {
				ptr::null_mut()
			} else {
				objects.as_mut_ptr()
			},
			objects.len(),
		)
	}
}

fn str_object(text: &str) -> *mut PyObject {
	unsafe { pon_const_str(text.as_ptr(), text.len()) }
}

unsafe fn arg_slice<'a>(argv: *mut *mut PyObject, argc: usize) -> Option<&'a [*mut PyObject]> {
	if argc == 0 {
		Some(&[])
	} else if argv.is_null() {
		None
	} else {
		Some(unsafe { std::slice::from_raw_parts(argv, argc) })
	}
}

fn string_arg(object: *mut PyObject, function: &str) -> Result<String, *mut PyObject> {
	let object = crate::tag::untag_arg(object);
	let Some(text) = (unsafe { unicode_text(object) }) else {
		return Err(raise_type_error(&format!("{function}() argument must be str")));
	};
	Ok(text.to_owned())
}

fn gid_arg(object: *mut PyObject) -> Result<libc::gid_t, *mut PyObject> {
	let value = int_arg(object, "gid")?;
	libc::gid_t::try_from(value)
		.map_err(|_| raise_key_error(&format!("getgrgid(): gid not found: {value}")))
}

fn int_arg(object: *mut PyObject, what: &str) -> Result<i64, *mut PyObject> {
	if crate::tag::is_small_int(object) {
		return Ok(crate::tag::untag_small_int(object));
	}
	let object = crate::tag::untag_arg(object);
	if object.is_null() {
		return Err(ptr::null_mut());
	}
	unsafe { crate::types::int::to_bigint_including_bool(object) }
		.and_then(|value| value.to_i64())
		.ok_or_else(|| raise_type_error(&format!("{what} must be an integer")))
}

fn raise_type_error(message: &str) -> *mut PyObject {
	abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message)
}

fn raise_value_error(message: &str) -> *mut PyObject {
	abi::exc::raise_kind_error_text(ExceptionKind::ValueError, message)
}

fn raise_key_error(message: &str) -> *mut PyObject {
	abi::exc::raise_kind_error_text(ExceptionKind::KeyError, message)
}

fn raise_index_error(message: &str) -> *mut PyObject {
	abi::exc::raise_kind_error_text(ExceptionKind::IndexError, message)
}
