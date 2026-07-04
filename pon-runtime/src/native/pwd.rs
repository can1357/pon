//! Native `pwd` module backed by the host password database.
//!
//! The lookups call the platform libc functions (`getpwnam`, `getpwuid`,
//! `getpwent`) and expose CPython-shaped `struct_passwd` objects with both
//! tuple-style indexing and named fields.

use std::ffi::{CStr, CString};
use std::ptr;
use std::sync::LazyLock;

use num_traits::ToPrimitive;

use crate::abi::{self, pon_const_int, pon_const_str, pon_make_function};
use crate::intern::intern;
use crate::object::{PyObject, PyObjectHeader, PySequenceMethods, PyType};
use crate::types::exc::ExceptionKind;
use crate::types::type_::unicode_text;

use super::install_module;

const VARIADIC_ARITY: usize = crate::native::builtins_mod::VARIADIC_ARITY;
const PASSWD_FIELDS: [&str; 7] = ["pw_name", "pw_passwd", "pw_uid", "pw_gid", "pw_gecos", "pw_dir", "pw_shell"];

#[derive(Clone, Debug)]
struct PasswdRecord {
    name: String,
    passwd: String,
    uid: i64,
    gid: i64,
    gecos: String,
    dir: String,
    shell: String,
}

#[repr(C)]
struct PyPasswd {
    ob_base: PyObjectHeader,
    record: PasswdRecord,
}

static PASSWD_SEQUENCE: LazyLock<PySequenceMethods> = LazyLock::new(|| PySequenceMethods {
    sq_length: Some(passwd_len),
    sq_item: Some(passwd_item),
    ..PySequenceMethods::EMPTY
});

static PASSWD_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(
        abi::runtime_type_type().cast_const(),
        "pwd.struct_passwd",
        std::mem::size_of::<PyPasswd>(),
    );
    ty.tp_as_sequence = &*PASSWD_SEQUENCE as *const PySequenceMethods as *mut PySequenceMethods;
    ty.tp_getattro = Some(passwd_getattro);
    ty.tp_repr = Some(passwd_repr);
    Box::into_raw(Box::new(ty)) as usize
});

fn passwd_type() -> *mut PyType {
    *PASSWD_TYPE as *mut PyType
}

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let name = "pwd";
    let mut attrs = vec![string_attr("__name__", name)?];
    attrs.push(function_attr("getpwnam", pwd_getpwnam)?);
    attrs.push(function_attr("getpwuid", pwd_getpwuid)?);
    attrs.push(function_attr("getpwall", pwd_getpwall)?);
    attrs.push((intern("struct_passwd"), passwd_type().cast::<PyObject>()));
    install_module(name, attrs)
}

fn string_attr(name: &str, value: &str) -> Result<(u32, *mut PyObject), String> {
    let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate pwd.{name}"))
}

fn function_attr(
    name: &str,
    entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> Result<(u32, *mut PyObject), String> {
    let function = unsafe { pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
    (!function.is_null())
        .then_some((intern(name), function))
        .ok_or_else(|| format!("failed to allocate pwd.{name}"))
}

unsafe extern "C" fn pwd_getpwnam(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { arg_slice(argv, argc) } {
        Some(args) if args.len() == 1 => args,
        _ => return raise_type_error(&format!("getpwnam() takes exactly 1 argument ({argc} given)")),
    };
    let name = match string_arg(args[0], "getpwnam") {
        Ok(name) => name,
        Err(error) => return error,
    };
    let c_name = match CString::new(name.as_str()) {
        Ok(value) => value,
        Err(_) => return raise_value_error("embedded null character"),
    };
    let entry = unsafe { libc::getpwnam(c_name.as_ptr()) };
    if entry.is_null() {
        return raise_key_error(&format!("getpwnam(): name not found: {name}"));
    }
    match unsafe { record_from_passwd(entry) } {
        Ok(record) => passwd_object(record),
        Err(error) => error,
    }
}

unsafe extern "C" fn pwd_getpwuid(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { arg_slice(argv, argc) } {
        Some(args) if args.len() == 1 => args,
        _ => return raise_type_error(&format!("getpwuid() takes exactly 1 argument ({argc} given)")),
    };
    let uid = match uid_arg(args[0]) {
        Ok(uid) => uid,
        Err(error) => return error,
    };
    let entry = unsafe { libc::getpwuid(uid) };
    if entry.is_null() {
        return raise_key_error(&format!("getpwuid(): uid not found: {uid}"));
    }
    match unsafe { record_from_passwd(entry) } {
        Ok(record) => passwd_object(record),
        Err(error) => error,
    }
}

unsafe extern "C" fn pwd_getpwall(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 0 || !argv.is_null() {
        return raise_type_error(&format!("getpwall() takes no arguments ({argc} given)"));
    }
    let mut items = Vec::new();
    unsafe { libc::setpwent() };
    loop {
        let entry = unsafe { libc::getpwent() };
        if entry.is_null() {
            break;
        }
        let record = match unsafe { record_from_passwd(entry) } {
            Ok(record) => record,
            Err(error) => {
                unsafe { libc::endpwent() };
                return error;
            }
        };
        let object = passwd_object(record);
        if object.is_null() {
            unsafe { libc::endpwent() };
            return ptr::null_mut();
        }
        items.push(object);
    }
    unsafe { libc::endpwent() };
    unsafe { abi::seq::pon_build_list(if items.is_empty() { ptr::null_mut() } else { items.as_mut_ptr() }, items.len()) }
}

unsafe fn record_from_passwd(entry: *mut libc::passwd) -> Result<PasswdRecord, *mut PyObject> {
    if entry.is_null() {
        return Err(raise_key_error("password entry not found"));
    }
    let raw = unsafe { &*entry };
    Ok(PasswdRecord {
        name: c_string(raw.pw_name),
        passwd: c_string(raw.pw_passwd),
        uid: i64::from(raw.pw_uid),
        gid: i64::from(raw.pw_gid),
        gecos: c_string(raw.pw_gecos),
        dir: c_string(raw.pw_dir),
        shell: c_string(raw.pw_shell),
    })
}

fn c_string(ptr: *const libc::c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
    }
}

fn passwd_object(record: PasswdRecord) -> *mut PyObject {
    Box::into_raw(Box::new(PyPasswd {
        ob_base: PyObjectHeader::new(passwd_type()),
        record,
    }))
    .cast::<PyObject>()
}

unsafe extern "C" fn passwd_len(_object: *mut PyObject) -> isize {
    PASSWD_FIELDS.len() as isize
}

unsafe extern "C" fn passwd_item(object: *mut PyObject, index: isize) -> *mut PyObject {
    let Ok(index) = usize::try_from(index) else {
        return raise_index_error("tuple index out of range");
    };
    let record = unsafe { &(*object.cast::<PyPasswd>()).record };
    passwd_field(record, index).unwrap_or_else(|| raise_index_error("tuple index out of range"))
}

unsafe extern "C" fn passwd_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name_text) = (unsafe { unicode_text(crate::tag::untag_arg(name)) }) else {
        return raise_type_error("attribute name must be str");
    };
    if name_text == "n_fields" || name_text == "n_sequence_fields" {
        return unsafe { pon_const_int(PASSWD_FIELDS.len() as i64) };
    }
    if name_text == "n_unnamed_fields" {
        return unsafe { pon_const_int(0) };
    }
    let record = unsafe { &(*object.cast::<PyPasswd>()).record };
    if let Some(index) = PASSWD_FIELDS.iter().position(|&field| field == name_text) {
        return passwd_field(record, index).unwrap_or(ptr::null_mut());
    }
    unsafe { abi::exc::pon_raise_attribute_error(object, intern(name_text)) }
}

unsafe extern "C" fn passwd_repr(object: *mut PyObject) -> *mut PyObject {
    let record = unsafe { &(*object.cast::<PyPasswd>()).record };
    let text = format!(
        "pwd.struct_passwd(pw_name={}, pw_passwd={}, pw_uid={}, pw_gid={}, pw_gecos={}, pw_dir={}, pw_shell={})",
        record.name.repr_quote(),
        record.passwd.repr_quote(),
        record.uid,
        record.gid,
        record.gecos.repr_quote(),
        record.dir.repr_quote(),
        record.shell.repr_quote()
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

fn passwd_field(record: &PasswdRecord, index: usize) -> Option<*mut PyObject> {
    match index {
        0 => Some(str_object(&record.name)),
        1 => Some(str_object(&record.passwd)),
        2 => Some(unsafe { pon_const_int(record.uid) }),
        3 => Some(unsafe { pon_const_int(record.gid) }),
        4 => Some(str_object(&record.gecos)),
        5 => Some(str_object(&record.dir)),
        6 => Some(str_object(&record.shell)),
        _ => None,
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

fn uid_arg(object: *mut PyObject) -> Result<libc::uid_t, *mut PyObject> {
    let value = int_arg(object, "uid")?;
    libc::uid_t::try_from(value).map_err(|_| raise_key_error(&format!("getpwuid(): uid not found: {value}")))
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
