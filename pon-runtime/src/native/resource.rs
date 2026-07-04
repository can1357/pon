//! Native `resource` module backed by POSIX `getrlimit`/`getrusage`.
//!
//! Constants are Darwin header values matching the host CPython oracle, while
//! functions call libc and return CPython-shaped tuples / `struct_rusage`.

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
const RUSAGE_FIELDS: [&str; 16] = [
    "ru_utime",
    "ru_stime",
    "ru_maxrss",
    "ru_ixrss",
    "ru_idrss",
    "ru_isrss",
    "ru_minflt",
    "ru_majflt",
    "ru_nswap",
    "ru_inblock",
    "ru_oublock",
    "ru_msgsnd",
    "ru_msgrcv",
    "ru_nsignals",
    "ru_nvcsw",
    "ru_nivcsw",
];

#[cfg(target_os = "macos")]
const CONSTANTS: &[(&str, i64)] = &[
    ("RLIMIT_AS", 5),
    ("RLIMIT_CORE", 4),
    ("RLIMIT_CPU", 0),
    ("RLIMIT_DATA", 2),
    ("RLIMIT_FSIZE", 1),
    ("RLIMIT_MEMLOCK", 6),
    ("RLIMIT_NOFILE", 8),
    ("RLIMIT_NPROC", 7),
    ("RLIMIT_RSS", 5),
    ("RLIMIT_STACK", 3),
    ("RLIM_INFINITY", i64::MAX),
    ("RUSAGE_CHILDREN", -1),
    ("RUSAGE_SELF", 0),
];

#[cfg(not(target_os = "macos"))]
const CONSTANTS: &[(&str, i64)] = &[
    ("RLIMIT_AS", libc::RLIMIT_AS as i64),
    ("RLIMIT_CORE", libc::RLIMIT_CORE as i64),
    ("RLIMIT_CPU", libc::RLIMIT_CPU as i64),
    ("RLIMIT_DATA", libc::RLIMIT_DATA as i64),
    ("RLIMIT_FSIZE", libc::RLIMIT_FSIZE as i64),
    ("RLIMIT_NOFILE", libc::RLIMIT_NOFILE as i64),
    ("RLIMIT_STACK", libc::RLIMIT_STACK as i64),
    ("RLIM_INFINITY", libc::RLIM_INFINITY as i64),
    ("RUSAGE_CHILDREN", libc::RUSAGE_CHILDREN as i64),
    ("RUSAGE_SELF", libc::RUSAGE_SELF as i64),
];

#[derive(Clone, Copy)]
struct RUsageRecord {
    values: [RUsageValue; 16],
}

#[derive(Clone, Copy)]
enum RUsageValue {
    Float(f64),
    Int(i64),
}

#[repr(C)]
struct PyRUsage {
    ob_base: PyObjectHeader,
    record: RUsageRecord,
}

static RUSAGE_SEQUENCE: LazyLock<PySequenceMethods> = LazyLock::new(|| PySequenceMethods {
    sq_length: Some(rusage_len),
    sq_item: Some(rusage_item),
    ..PySequenceMethods::EMPTY
});

static RUSAGE_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(
        abi::runtime_type_type().cast_const(),
        "resource.struct_rusage",
        std::mem::size_of::<PyRUsage>(),
    );
    ty.tp_as_sequence = &*RUSAGE_SEQUENCE as *const PySequenceMethods as *mut PySequenceMethods;
    ty.tp_getattro = Some(rusage_getattro);
    Box::into_raw(Box::new(ty)) as usize
});

fn rusage_type() -> *mut PyType {
    *RUSAGE_TYPE as *mut PyType
}

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let name = "resource";
    let mut attrs = vec![string_attr("__name__", name)?];
    attrs.push((intern("error"), builtin_os_error()?));
    attrs.push((intern("struct_rusage"), rusage_type().cast::<PyObject>()));
    for &(const_name, value) in CONSTANTS {
        attrs.push(int_attr(const_name, value)?);
    }
    attrs.push(function_attr("getpagesize", resource_getpagesize)?);
    attrs.push(function_attr("getrlimit", resource_getrlimit)?);
    attrs.push(function_attr("setrlimit", resource_setrlimit)?);
    attrs.push(function_attr("getrusage", resource_getrusage)?);
    install_module(name, attrs)
}

fn builtin_os_error() -> Result<*mut PyObject, String> {
    crate::import::module_attr(intern("builtins"), intern("OSError"))
        .ok_or_else(|| "failed to resolve builtins.OSError".to_owned())
}

fn string_attr(name: &str, value: &str) -> Result<(u32, *mut PyObject), String> {
    let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate resource.{name}"))
}

fn int_attr(name: &str, value: i64) -> Result<(u32, *mut PyObject), String> {
    let object = unsafe { pon_const_int(value) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate resource.{name}"))
}

fn function_attr(
    name: &str,
    entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> Result<(u32, *mut PyObject), String> {
    let function = unsafe { pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
    (!function.is_null())
        .then_some((intern(name), function))
        .ok_or_else(|| format!("failed to allocate resource.{name}"))
}

unsafe extern "C" fn resource_getpagesize(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 0 || !argv.is_null() {
        return raise_type_error(&format!("getpagesize() takes no arguments ({argc} given)"));
    }
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if size <= 0 {
        return raise_errno();
    }
    unsafe { pon_const_int(size as i64) }
}

unsafe extern "C" fn resource_getrlimit(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { arg_slice(argv, argc) } {
        Some(args) if args.len() == 1 => args,
        _ => return raise_type_error(&format!("getrlimit expected 1 argument, got {argc}")),
    };
    let resource = match resource_arg(args[0]) {
        Ok(resource) => resource,
        Err(error) => return error,
    };
    let mut limit = std::mem::MaybeUninit::<libc::rlimit>::uninit();
    if unsafe { libc::getrlimit(resource, limit.as_mut_ptr()) } != 0 {
        return raise_errno();
    }
    let limit = unsafe { limit.assume_init() };
    let mut values = [rlim_object(limit.rlim_cur), rlim_object(limit.rlim_max)];
    unsafe { abi::seq::pon_build_tuple(values.as_mut_ptr(), values.len()) }
}

unsafe extern "C" fn resource_setrlimit(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { arg_slice(argv, argc) } {
        Some(args) if args.len() == 2 => args,
        _ => return raise_type_error(&format!("setrlimit expected 2 arguments, got {argc}")),
    };
    let resource = match resource_arg(args[0]) {
        Ok(resource) => resource,
        Err(error) => return error,
    };
    let limits = match sequence_items(args[1], "limits") {
        Ok(items) if items.len() == 2 => items,
        Ok(_) => return raise_type_error("setrlimit() argument 2 must be a 2-item sequence"),
        Err(error) => return error,
    };
    let limit = libc::rlimit {
        rlim_cur: match rlim_arg(limits[0]) {
            Ok(value) => value,
            Err(error) => return error,
        },
        rlim_max: match rlim_arg(limits[1]) {
            Ok(value) => value,
            Err(error) => return error,
        },
    };
    if unsafe { libc::setrlimit(resource, &limit) } != 0 {
        return raise_errno();
    }
    unsafe { abi::pon_none() }
}

unsafe extern "C" fn resource_getrusage(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { arg_slice(argv, argc) } {
        Some(args) if args.len() == 1 => args,
        _ => return raise_type_error(&format!("getrusage expected 1 argument, got {argc}")),
    };
    let who = match c_int_arg(args[0], "who") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    if unsafe { libc::getrusage(who, usage.as_mut_ptr()) } != 0 {
        return raise_errno();
    }
    let usage = unsafe { usage.assume_init() };
    rusage_object(rusage_record(&usage))
}

fn rusage_record(usage: &libc::rusage) -> RUsageRecord {
    RUsageRecord {
        values: [
            RUsageValue::Float(timeval_seconds(usage.ru_utime)),
            RUsageValue::Float(timeval_seconds(usage.ru_stime)),
            RUsageValue::Int(usage.ru_maxrss as i64),
            RUsageValue::Int(usage.ru_ixrss as i64),
            RUsageValue::Int(usage.ru_idrss as i64),
            RUsageValue::Int(usage.ru_isrss as i64),
            RUsageValue::Int(usage.ru_minflt as i64),
            RUsageValue::Int(usage.ru_majflt as i64),
            RUsageValue::Int(usage.ru_nswap as i64),
            RUsageValue::Int(usage.ru_inblock as i64),
            RUsageValue::Int(usage.ru_oublock as i64),
            RUsageValue::Int(usage.ru_msgsnd as i64),
            RUsageValue::Int(usage.ru_msgrcv as i64),
            RUsageValue::Int(usage.ru_nsignals as i64),
            RUsageValue::Int(usage.ru_nvcsw as i64),
            RUsageValue::Int(usage.ru_nivcsw as i64),
        ],
    }
}

fn timeval_seconds(value: libc::timeval) -> f64 {
    value.tv_sec as f64 + value.tv_usec as f64 * 1e-6
}

fn rusage_object(record: RUsageRecord) -> *mut PyObject {
    Box::into_raw(Box::new(PyRUsage {
        ob_base: PyObjectHeader::new(rusage_type()),
        record,
    }))
    .cast::<PyObject>()
}

unsafe extern "C" fn rusage_len(_object: *mut PyObject) -> isize {
    RUSAGE_FIELDS.len() as isize
}

unsafe extern "C" fn rusage_item(object: *mut PyObject, index: isize) -> *mut PyObject {
    let Ok(index) = usize::try_from(index) else {
        return raise_index_error("tuple index out of range");
    };
    let record = unsafe { &(*object.cast::<PyRUsage>()).record };
    record.values.get(index).map_or_else(|| raise_index_error("tuple index out of range"), |&value| rusage_value_object(value))
}

unsafe extern "C" fn rusage_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name_text) = (unsafe { unicode_text(crate::tag::untag_arg(name)) }) else {
        return raise_type_error("attribute name must be str");
    };
    if name_text == "n_fields" || name_text == "n_sequence_fields" {
        return unsafe { pon_const_int(RUSAGE_FIELDS.len() as i64) };
    }
    if name_text == "n_unnamed_fields" {
        return unsafe { pon_const_int(0) };
    }
    let record = unsafe { &(*object.cast::<PyRUsage>()).record };
    if let Some(index) = RUSAGE_FIELDS.iter().position(|&field| field == name_text) {
        return rusage_value_object(record.values[index]);
    }
    unsafe { abi::exc::pon_raise_attribute_error(object, intern(name_text)) }
}

fn rusage_value_object(value: RUsageValue) -> *mut PyObject {
    match value {
        RUsageValue::Float(value) => unsafe { abi::number::pon_const_float(value) },
        RUsageValue::Int(value) => unsafe { pon_const_int(value) },
    }
}

fn rlim_object(value: libc::rlim_t) -> *mut PyObject {
    if value == libc::RLIM_INFINITY {
        unsafe { pon_const_int(i64::MAX) }
    } else {
        unsafe { pon_const_int(i64::try_from(value).unwrap_or(i64::MAX)) }
    }
}

fn sequence_items(object: *mut PyObject, what: &str) -> Result<Vec<*mut PyObject>, *mut PyObject> {
    let object = crate::tag::untag_arg(object);
    if object.is_null() {
        return Err(ptr::null_mut());
    }
    match unsafe { crate::types::dict::type_name(object) } {
        Some("list") => {
            let list = unsafe { &*object.cast::<crate::types::list::PyList>() };
            Ok(unsafe { list.as_slice() }.to_vec())
        }
        Some("tuple") => {
            let tuple = unsafe { &*object.cast::<crate::types::tuple::PyTuple>() };
            Ok(unsafe { tuple.as_slice() }.to_vec())
        }
        _ => Err(raise_type_error(&format!("{what} must be a sequence"))),
    }
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

fn resource_arg(object: *mut PyObject) -> Result<libc::c_int, *mut PyObject> {
    let value = int_arg(object, "resource")?;
    libc::c_int::try_from(value).map_err(|_| raise_value_error("invalid resource specified"))
}

fn c_int_arg(object: *mut PyObject, what: &str) -> Result<libc::c_int, *mut PyObject> {
    let value = int_arg(object, what)?;
    libc::c_int::try_from(value).map_err(|_| raise_value_error(&format!("{what} is out of range")))
}

fn rlim_arg(object: *mut PyObject) -> Result<libc::rlim_t, *mut PyObject> {
    let value = int_arg(object, "limit")?;
    if value < 0 || value == i64::MAX {
        return Ok(libc::RLIM_INFINITY);
    }
    libc::rlim_t::try_from(value).map_err(|_| raise_value_error("current limit exceeds maximum limit"))
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

fn raise_index_error(message: &str) -> *mut PyObject {
    abi::exc::raise_kind_error_text(ExceptionKind::IndexError, message)
}

fn raise_errno() -> *mut PyObject {
    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO);
    super::os::raise_errno(errno, None)
}
