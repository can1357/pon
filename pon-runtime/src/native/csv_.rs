//! Native `_csv` accelerator subset for the vendored `csv` module.
//!
//! The implementation keeps the surface deliberately small but CPython-shaped:
//! module-level dialect registration, immutable dialect objects, writer/reader
//! native objects, and a catchable `_csv.Error` exception type.

use core::ffi::c_int;
use core::ptr;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{LazyLock, Mutex};

use num_traits::ToPrimitive;

use crate::abi::{self, CodeInfo, ParamSpec, pon_get_iter, pon_iter_next};
use crate::intern::intern;
use crate::object::{PyObject, PyObjectHeader, PyType};
use crate::thread_state::{pon_err_clear, pon_err_occurred};
use crate::types::exc::{ExceptionKind, PyBaseException};
use crate::types::type_::unicode_text;

use super::builtins_mod::{alloc_list, try_str_text, VARIADIC_ARITY};
use super::install_module;

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

const QUOTE_MINIMAL: i64 = 0;
const QUOTE_ALL: i64 = 1;
const QUOTE_NONNUMERIC: i64 = 2;
const QUOTE_NONE: i64 = 3;
const QUOTE_STRINGS: i64 = 4;
const QUOTE_NOTNULL: i64 = 5;

const DEFAULT_FIELD_SIZE_LIMIT: i64 = 131_072;
static FIELD_SIZE_LIMIT: AtomicI64 = AtomicI64::new(DEFAULT_FIELD_SIZE_LIMIT);
static DIALECTS: LazyLock<Mutex<BTreeMap<String, DialectConfig>>> = LazyLock::new(|| Mutex::new(BTreeMap::new()));
static DIALECTS_DICT: Mutex<usize> = Mutex::new(0);

#[derive(Clone, Debug)]
struct DialectConfig {
    delimiter: char,
    quotechar: Option<char>,
    escapechar: Option<char>,
    doublequote: bool,
    skipinitialspace: bool,
    lineterminator: String,
    quoting: i64,
    strict: bool,
}

impl Default for DialectConfig {
    fn default() -> Self {
        Self {
            delimiter: ',',
            quotechar: Some('"'),
            escapechar: None,
            doublequote: true,
            skipinitialspace: false,
            lineterminator: "\r\n".to_owned(),
            quoting: QUOTE_MINIMAL,
            strict: false,
        }
    }
}

#[repr(C)]
struct CsvDialect {
    ob_base: PyObjectHeader,
    config: DialectConfig,
}

#[repr(C)]
struct CsvWriter {
    ob_base: PyObjectHeader,
    file: *mut PyObject,
    dialect: DialectConfig,
}

#[repr(C)]
struct CsvReader {
    ob_base: PyObjectHeader,
    iter: *mut PyObject,
    dialect: DialectConfig,
    line_num: usize,
}

#[derive(Debug)]
enum ConfigError {
    Type(String),
    Csv(String),
    Raised,
}

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let name = "_csv";
    let mut attrs = vec![
        string_attr("__name__", name)?,
        string_attr("__version__", "1.0")?,
        (intern("Error"), csv_error_type().cast::<PyObject>()),
        (intern("Dialect"), dialect_type().cast::<PyObject>()),
        (intern("Reader"), reader_type().cast::<PyObject>()),
        (intern("Writer"), writer_type().cast::<PyObject>()),
        (intern("_dialects"), dialects_dict()),
    ];

    for &(const_name, value) in &[
        ("QUOTE_MINIMAL", QUOTE_MINIMAL),
        ("QUOTE_ALL", QUOTE_ALL),
        ("QUOTE_NONNUMERIC", QUOTE_NONNUMERIC),
        ("QUOTE_NONE", QUOTE_NONE),
        ("QUOTE_STRINGS", QUOTE_STRINGS),
        ("QUOTE_NOTNULL", QUOTE_NOTNULL),
    ] {
        attrs.push(int_attr(const_name, value)?);
    }

    let none = none();
    let excel = alloc_str_object("excel");
    if none.is_null() || excel.is_null() {
        return Err("failed to allocate _csv function defaults".to_owned());
    }

    attrs.push(phase_b_function_attr(
        "field_size_limit",
        field_size_limit_entry,
        &["new_limit"],
        1,
        &mut [none],
        None,
    )?);
    attrs.push(phase_b_function_attr(
        "register_dialect",
        register_dialect_entry,
        &["name", "dialect"],
        2,
        &mut [none],
        Some("fmtparams"),
    )?);
    attrs.push(phase_b_function_attr(
        "unregister_dialect",
        unregister_dialect_entry,
        &["name"],
        1,
        &mut [],
        None,
    )?);
    attrs.push(phase_b_function_attr(
        "get_dialect",
        get_dialect_entry,
        &["name"],
        1,
        &mut [],
        None,
    )?);
    attrs.push(phase_b_function_attr(
        "list_dialects",
        list_dialects_entry,
        &[],
        0,
        &mut [],
        None,
    )?);
    attrs.push(phase_b_function_attr(
        "writer",
        writer_entry,
        &["fileobj", "dialect"],
        2,
        &mut [excel],
        Some("fmtparams"),
    )?);
    let excel = alloc_str_object("excel");
    if excel.is_null() {
        return Err("failed to allocate _csv.reader default dialect".to_owned());
    }
    attrs.push(phase_b_function_attr(
        "reader",
        reader_entry,
        &["iterable", "dialect"],
        2,
        &mut [excel],
        Some("fmtparams"),
    )?);

    install_module(name, attrs)
}

fn string_attr(name: &str, value: &str) -> Result<(u32, *mut PyObject), String> {
    let object = alloc_str_object(value);
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate _csv.{name}"))
}

fn int_attr(name: &str, value: i64) -> Result<(u32, *mut PyObject), String> {
    let object = alloc_int_object(value);
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate _csv.{name}"))
}

fn phase_b_function_attr(
    name: &str,
    entry: BuiltinFn,
    names: &[&str],
    positional_count: u32,
    defaults: &mut [*mut PyObject],
    varkw: Option<&str>,
) -> Result<(u32, *mut PyObject), String> {
    let interned_names: Vec<u32> = names.iter().map(|name| intern(name)).collect();
    let params = ParamSpec {
        names: if interned_names.is_empty() {
            ptr::null()
        } else {
            interned_names.as_ptr()
        },
        total_param_count: interned_names.len() as u32,
        positional_only_count: 0,
        positional_count,
        keyword_only_count: 0,
        varargs_name: 0,
        varkw_name: varkw.map_or(0, intern),
    };
    let code = CodeInfo {
        entry: entry as *const u8,
        params: &params,
        name_interned: intern(name),
        n_locals: 0,
        n_feedback: 0,
        flags: 0,
    };
    let function = unsafe {
        abi::call::pon_make_function_full(
            &code,
            if defaults.is_empty() { ptr::null_mut() } else { defaults.as_mut_ptr() },
            defaults.len(),
            ptr::null(),
            ptr::null_mut(),
            0,
            ptr::null(),
            ptr::null_mut(),
            0,
        )
    };
    if function.is_null() {
        return Err(format!("failed to allocate _csv.{name}"));
    }
    crate::types::function::mark_native_function(function);
    Ok((intern(name), function))
}

// ---------------------------------------------------------------------------
// Type objects and allocation

fn csv_error_type() -> *mut PyType {
    static TYPE: LazyLock<usize> = LazyLock::new(|| {
        let base = crate::import::module_attr(intern("builtins"), intern("Exception"))
            .map_or(ptr::null_mut(), |object| object.cast::<PyType>());
        let mut ty = PyType::new(
            abi::runtime_type_type().cast_const(),
            "_csv.Error",
            std::mem::size_of::<PyBaseException>(),
        );
        ty.tp_base = base;
        ty.tp_getattro = Some(crate::types::exc::exception_getattro);
        ty.tp_setattro = Some(crate::types::exc::exception_setattro);
        Box::into_raw(Box::new(ty)) as usize
    });
    *TYPE as *mut PyType
}

fn dialect_type() -> *mut PyType {
    static TYPE: LazyLock<usize> = LazyLock::new(|| {
        let mut ty = PyType::new(
            abi::runtime_type_type().cast_const(),
            "_csv.Dialect",
            std::mem::size_of::<CsvDialect>(),
        );
        ty.tp_base = runtime_object_type();
        ty.tp_new = Some(dialect_new);
        ty.tp_getattro = Some(dialect_getattro);
        ty.tp_hash = Some(identity_hash);
        ty.tp_bool = Some(always_true);
        Box::into_raw(Box::new(ty)) as usize
    });
    *TYPE as *mut PyType
}

fn writer_type() -> *mut PyType {
    static TYPE: LazyLock<usize> = LazyLock::new(|| {
        let mut ty = PyType::new(
            abi::runtime_type_type().cast_const(),
            "_csv.writer",
            std::mem::size_of::<CsvWriter>(),
        );
        ty.tp_getattro = Some(writer_getattro);
        ty.tp_hash = Some(identity_hash);
        ty.tp_bool = Some(always_true);
        Box::into_raw(Box::new(ty)) as usize
    });
    *TYPE as *mut PyType
}

fn reader_type() -> *mut PyType {
    static TYPE: LazyLock<usize> = LazyLock::new(|| {
        let mut ty = PyType::new(
            abi::runtime_type_type().cast_const(),
            "_csv.reader",
            std::mem::size_of::<CsvReader>(),
        );
        ty.tp_getattro = Some(reader_getattro);
        ty.tp_iter = Some(identity_slot);
        ty.tp_iternext = Some(reader_next_slot);
        ty.tp_hash = Some(identity_hash);
        ty.tp_bool = Some(always_true);
        Box::into_raw(Box::new(ty)) as usize
    });
    *TYPE as *mut PyType
}

fn runtime_object_type() -> *mut PyType {
    abi::runtime_global(intern("object")).map_or(ptr::null_mut(), |object| object.cast::<PyType>())
}

fn alloc_dialect(config: DialectConfig) -> *mut PyObject {
    Box::into_raw(Box::new(CsvDialect {
        ob_base: PyObjectHeader::new(dialect_type()),
        config,
    }))
    .cast::<PyObject>()
}

fn alloc_writer(file: *mut PyObject, dialect: DialectConfig) -> *mut PyObject {
    Box::into_raw(Box::new(CsvWriter {
        ob_base: PyObjectHeader::new(writer_type()),
        file,
        dialect,
    }))
    .cast::<PyObject>()
}

fn alloc_reader(iter: *mut PyObject, dialect: DialectConfig) -> *mut PyObject {
    Box::into_raw(Box::new(CsvReader {
        ob_base: PyObjectHeader::new(reader_type()),
        iter,
        dialect,
        line_num: 0,
    }))
    .cast::<PyObject>()
}

unsafe fn as_dialect<'a>(object: *mut PyObject) -> Option<&'a CsvDialect> {
    let object = untag(object);
    if object.is_null() || unsafe { (*object).ob_type } != dialect_type().cast_const() {
        return None;
    }
    Some(unsafe { &*object.cast::<CsvDialect>() })
}

unsafe fn as_writer<'a>(object: *mut PyObject) -> Option<&'a mut CsvWriter> {
    let object = untag(object);
    if object.is_null() || unsafe { (*object).ob_type } != writer_type().cast_const() {
        return None;
    }
    Some(unsafe { &mut *object.cast::<CsvWriter>() })
}

unsafe fn as_reader<'a>(object: *mut PyObject) -> Option<&'a mut CsvReader> {
    let object = untag(object);
    if object.is_null() || unsafe { (*object).ob_type } != reader_type().cast_const() {
        return None;
    }
    Some(unsafe { &mut *object.cast::<CsvReader>() })
}

unsafe extern "C" fn identity_hash(object: *mut PyObject) -> isize {
    object.addr() as isize
}

unsafe extern "C" fn always_true(_object: *mut PyObject) -> c_int {
    1
}

unsafe extern "C" fn identity_slot(object: *mut PyObject) -> *mut PyObject {
    object
}

// ---------------------------------------------------------------------------
// Generic helpers

fn untag(object: *mut PyObject) -> *mut PyObject {
    crate::tag::untag_arg(object)
}

fn none() -> *mut PyObject {
    unsafe { abi::pon_none() }
}

fn is_none(object: *mut PyObject) -> bool {
    unsafe { crate::types::dict::type_name(untag(object)) == Some("NoneType") }
}

fn alloc_str_object(text: &str) -> *mut PyObject {
    unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

fn alloc_int_object(value: i64) -> *mut PyObject {
    unsafe { abi::pon_const_int(value) }
}

fn alloc_bool_object(value: bool) -> *mut PyObject {
    unsafe { abi::pon_const_bool(c_int::from(value)) }
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

fn to_i64(object: *mut PyObject) -> Option<i64> {
    let object = untag(object);
    if object.is_null() {
        return None;
    }
    unsafe { crate::types::int::to_bigint_including_bool(object) }.and_then(|value| value.to_i64())
}

fn is_number(object: *mut PyObject) -> bool {
    let object = untag(object);
    if object.is_null() {
        return false;
    }
    unsafe { crate::types::int::to_bigint_including_bool(object).is_some() || crate::types::float::is_exact_float(object) }
}

fn is_string(object: *mut PyObject) -> bool {
    unsafe { unicode_text(untag(object)).is_some() }
}

fn raise_type_error(message: &str) -> *mut PyObject {
    abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message)
}

fn raise_csv_error(message: &str) -> *mut PyObject {
    let message_obj = alloc_str_object(message);
    if message_obj.is_null() {
        return ptr::null_mut();
    }
    let mut args = [message_obj];
    let exception = unsafe { abi::pon_call(csv_error_type().cast::<PyObject>(), args.as_mut_ptr(), args.len()) };
    if exception.is_null() {
        return ptr::null_mut();
    }
    unsafe { abi::pon_raise(exception, ptr::null_mut()) }
}

fn raise_config_error(error: ConfigError) -> *mut PyObject {
    match error {
        ConfigError::Type(message) => raise_type_error(&message),
        ConfigError::Csv(message) => raise_csv_error(&message),
        ConfigError::Raised => ptr::null_mut(),
    }
}

fn bound_method(receiver: *mut PyObject, name: &str, entry: BuiltinFn) -> *mut PyObject {
    let function = unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
    if function.is_null() {
        return ptr::null_mut();
    }
    match crate::types::method::new_bound_method(function, receiver) {
        Ok(method) => method.cast::<PyObject>(),
        Err(message) => abi::return_null_with_error(message),
    }
}

fn registry_get(name: &str) -> Option<DialectConfig> {
    DIALECTS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(name)
        .cloned()
}

fn dialects_dict() -> *mut PyObject {
    let mut slot = DIALECTS_DICT.lock().unwrap_or_else(|poison| poison.into_inner());
    if *slot == 0 {
        // SAFETY: A NULL pointer with zero pairs is the empty-dict builder contract.
        let object = unsafe { abi::map::pon_build_map(ptr::null_mut(), 0) };
        if object.is_null() {
            return object;
        }
        *slot = object as usize;
    }
    *slot as *mut PyObject
}

fn dialects_dict_insert(name: &str, config: &DialectConfig) {
    let dict = dialects_dict();
    if dict.is_null() {
        return;
    }
    let key = alloc_str_object(name);
    let value = alloc_dialect(config.clone());
    if key.is_null() || value.is_null() {
        return;
    }
    let _guard = crate::sync::begin_critical_section(dict);
    let _ = unsafe { crate::types::dict::dict_insert(dict, key, value) };
}

fn dialects_dict_remove(name: &str) {
    let dict = dialects_dict();
    if dict.is_null() {
        return;
    }
    let key = alloc_str_object(name);
    if key.is_null() {
        return;
    }
    let _guard = crate::sync::begin_critical_section(dict);
    let _ = unsafe { crate::types::dict::dict_remove(dict, key) };
}

fn registry_insert(name: String, config: DialectConfig) {
    DIALECTS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(name.clone(), config.clone());
    dialects_dict_insert(&name, &config);
}

fn registry_remove(name: &str) -> bool {
    let removed = DIALECTS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .remove(name)
        .is_some();
    if removed {
        dialects_dict_remove(name);
    }
    removed
}

// ---------------------------------------------------------------------------
// Dialect conversion and validation

fn parse_char_param(value: *mut PyObject, name: &str, allow_none: bool) -> Result<Option<char>, ConfigError> {
    let value = untag(value);
    if is_none(value) {
        return if allow_none {
            Ok(None)
        } else {
            Err(ConfigError::Type(format!("\"{name}\" must be string, not NoneType")))
        };
    }
    let Some(text) = (unsafe { unicode_text(value) }) else {
        return Err(ConfigError::Type(format!("\"{name}\" must be string")));
    };
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return Err(ConfigError::Type(format!("\"{name}\" must be a 1-character string")));
    };
    if chars.next().is_some() {
        return Err(ConfigError::Type(format!("\"{name}\" must be a 1-character string")));
    }
    Ok(Some(first))
}

fn parse_string_param(value: *mut PyObject, name: &str) -> Result<String, ConfigError> {
    let value = untag(value);
    let Some(text) = (unsafe { unicode_text(value) }) else {
        return Err(ConfigError::Type(format!("\"{name}\" must be a string")));
    };
    Ok(text.to_owned())
}

fn parse_bool_param(value: *mut PyObject, _name: &str) -> Result<bool, ConfigError> {
    let truth = unsafe { abi::pon_is_true(untag(value)) };
    if truth < 0 {
        return Err(ConfigError::Raised);
    }
    Ok(truth != 0)
}

fn parse_quoting(value: *mut PyObject) -> Result<i64, ConfigError> {
    let Some(value) = to_i64(value) else {
        return Err(ConfigError::Type("\"quoting\" must be an integer".to_owned()));
    };
    if !(QUOTE_MINIMAL..=QUOTE_NOTNULL).contains(&value) {
        return Err(ConfigError::Type("bad \"quoting\" value".to_owned()));
    }
    Ok(value)
}

fn apply_fmtparam(config: &mut DialectConfig, name: &str, value: *mut PyObject) -> Result<(), ConfigError> {
    match name {
        "delimiter" => {
            config.delimiter = parse_char_param(value, name, false)?.expect("delimiter rejects None");
        }
        "quotechar" => config.quotechar = parse_char_param(value, name, true)?,
        "escapechar" => config.escapechar = parse_char_param(value, name, true)?,
        "doublequote" => config.doublequote = parse_bool_param(value, name)?,
        "skipinitialspace" => config.skipinitialspace = parse_bool_param(value, name)?,
        "lineterminator" => config.lineterminator = parse_string_param(value, name)?,
        "quoting" => config.quoting = parse_quoting(value)?,
        "strict" => config.strict = parse_bool_param(value, name)?,
        other => return Err(ConfigError::Type(format!("'{other}' is an invalid keyword argument for this function"))),
    }
    validate_dialect(config)
}

fn validate_dialect(config: &DialectConfig) -> Result<(), ConfigError> {
    if config.quoting != QUOTE_NONE && config.quotechar.is_none() {
        return Err(ConfigError::Type("quotechar must be set if quoting enabled".to_owned()));
    }
    Ok(())
}

unsafe fn optional_attr(object: *mut PyObject, name: &str) -> Result<Option<*mut PyObject>, ConfigError> {
    let value = unsafe { abi::pon_get_attr(object, intern(name), ptr::null_mut()) };
    if value.is_null() {
        if abi::exc::pending_exception_is("AttributeError") {
            pon_err_clear();
            return Ok(None);
        }
        return Err(ConfigError::Raised);
    }
    Ok(Some(untag(value)))
}

fn config_from_object(object: *mut PyObject) -> Result<DialectConfig, ConfigError> {
    let object = untag(object);
    if let Some(dialect) = unsafe { as_dialect(object) } {
        return Ok(dialect.config.clone());
    }
    if let Some(name) = unsafe { unicode_text(object) } {
        if let Some(config) = registry_get(name) {
            return Ok(config);
        }
        if name == "excel" {
            return Ok(DialectConfig::default());
        }
        return Err(ConfigError::Csv("unknown dialect".to_owned()));
    }

    let mut config = DialectConfig::default();
    for attr in [
        "delimiter",
        "quotechar",
        "escapechar",
        "doublequote",
        "skipinitialspace",
        "lineterminator",
        "quoting",
        "strict",
    ] {
        if let Some(value) = unsafe { optional_attr(object, attr)? } {
            apply_fmtparam(&mut config, attr, value)?;
        }
    }
    validate_dialect(&config)?;
    Ok(config)
}

fn fmtparams_from_kwargs(kwargs: *mut PyObject) -> Result<Vec<(String, *mut PyObject)>, ConfigError> {
    if kwargs.is_null() || is_none(kwargs) {
        return Ok(Vec::new());
    }
    let entries = unsafe { crate::types::dict::dict_entries_snapshot(kwargs) }.map_err(ConfigError::Type)?;
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(name) = (unsafe { unicode_text(untag(entry.key)) }) else {
            return Err(ConfigError::Type("keywords must be strings".to_owned()));
        };
        out.push((name.to_owned(), untag(entry.value)));
    }
    Ok(out)
}

fn config_from_dialect_and_kwargs(dialect: *mut PyObject, kwargs: *mut PyObject) -> Result<DialectConfig, ConfigError> {
    let mut config = if is_none(dialect) {
        DialectConfig::default()
    } else {
        config_from_object(dialect)?
    };
    for (name, value) in fmtparams_from_kwargs(kwargs)? {
        apply_fmtparam(&mut config, &name, value)?;
    }
    validate_dialect(&config)?;
    Ok(config)
}

// ---------------------------------------------------------------------------
// Module-level functions

unsafe extern "C" fn field_size_limit_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return abi::return_null_with_error("field_size_limit() received a null argv pointer");
    };
    if args.len() != 1 {
        return raise_type_error(&format!("field_size_limit() takes at most 1 argument ({} given)", args.len()));
    }
    let old = FIELD_SIZE_LIMIT.load(Ordering::Relaxed);
    if !is_none(args[0]) {
        let Some(limit) = to_i64(args[0]) else {
            return raise_type_error("limit must be an integer");
        };
        FIELD_SIZE_LIMIT.store(limit, Ordering::Relaxed);
    }
    alloc_int_object(old)
}

unsafe extern "C" fn register_dialect_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return abi::return_null_with_error("register_dialect() received a null argv pointer");
    };
    if args.len() != 3 {
        return raise_type_error(&format!("register_dialect() expected 3 bound arguments, got {}", args.len()));
    }
    let Some(name) = (unsafe { unicode_text(untag(args[0])) }) else {
        return raise_type_error("dialect name must be a string");
    };
    match config_from_dialect_and_kwargs(args[1], args[2]) {
        Ok(config) => {
            registry_insert(name.to_owned(), config);
            none()
        }
        Err(error) => raise_config_error(error),
    }
}

unsafe extern "C" fn unregister_dialect_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return abi::return_null_with_error("unregister_dialect() received a null argv pointer");
    };
    if args.len() != 1 {
        return raise_type_error(&format!("unregister_dialect() takes exactly 1 argument ({} given)", args.len()));
    }
    let Some(name) = (unsafe { unicode_text(untag(args[0])) }) else {
        return raise_type_error("dialect name must be a string");
    };
    if registry_remove(name) {
        none()
    } else {
        raise_csv_error("unknown dialect")
    }
}

unsafe extern "C" fn get_dialect_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return abi::return_null_with_error("get_dialect() received a null argv pointer");
    };
    if args.len() != 1 {
        return raise_type_error(&format!("get_dialect() takes exactly 1 argument ({} given)", args.len()));
    }
    let Some(name) = (unsafe { unicode_text(untag(args[0])) }) else {
        return raise_type_error("dialect name must be a string");
    };
    match registry_get(name) {
        Some(config) => alloc_dialect(config),
        None => raise_csv_error("unknown dialect"),
    }
}

unsafe extern "C" fn list_dialects_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return abi::return_null_with_error("list_dialects() received a null argv pointer");
    };
    if !args.is_empty() {
        return raise_type_error(&format!("list_dialects() takes no arguments ({} given)", args.len()));
    }
    let names: Vec<String> = DIALECTS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .keys()
        .cloned()
        .collect();
    let mut objects = Vec::with_capacity(names.len());
    for name in names {
        let object = alloc_str_object(&name);
        if object.is_null() {
            return ptr::null_mut();
        }
        objects.push(object);
    }
    alloc_list(objects)
}

unsafe extern "C" fn writer_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return abi::return_null_with_error("writer() received a null argv pointer");
    };
    if args.len() != 3 {
        return raise_type_error(&format!("writer() expected 3 bound arguments, got {}", args.len()));
    }
    match config_from_dialect_and_kwargs(args[1], args[2]) {
        Ok(config) => alloc_writer(untag(args[0]), config),
        Err(error) => raise_config_error(error),
    }
}

unsafe extern "C" fn reader_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return abi::return_null_with_error("reader() received a null argv pointer");
    };
    if args.len() != 3 {
        return raise_type_error(&format!("reader() expected 3 bound arguments, got {}", args.len()));
    }
    let iter = unsafe { pon_get_iter(untag(args[0]), ptr::null_mut()) };
    if iter.is_null() {
        return ptr::null_mut();
    }
    match config_from_dialect_and_kwargs(args[1], args[2]) {
        Ok(config) => alloc_reader(untag(iter), config),
        Err(error) => raise_config_error(error),
    }
}

// ---------------------------------------------------------------------------
// Dialect type

unsafe extern "C" fn dialect_new(_cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    let positional = match unsafe { crate::types::type_::positional_args_from_object(args) } {
        Ok(positional) => positional,
        Err(message) => return raise_type_error(&message),
    };
    if positional.len() > 1 {
        return raise_type_error(&format!("Dialect() takes at most 1 positional argument ({} given)", positional.len()));
    }
    let dialect = positional.first().copied().unwrap_or_else(none);
    match config_from_dialect_and_kwargs(dialect, kwargs) {
        Ok(config) => alloc_dialect(config),
        Err(error) => raise_config_error(error),
    }
}

unsafe extern "C" fn dialect_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name_text) = (unsafe { unicode_text(untag(name)) }) else {
        return raise_type_error("attribute name must be str");
    };
    let Some(dialect) = (unsafe { as_dialect(object) }) else {
        return raise_type_error("_csv.Dialect receiver is invalid");
    };
    dialect_attr(&dialect.config, object, name_text)
}

fn dialect_attr(config: &DialectConfig, owner: *mut PyObject, name: &str) -> *mut PyObject {
    match name {
        "delimiter" => alloc_str_object(&config.delimiter.to_string()),
        "quotechar" => config
            .quotechar
            .map_or_else(none, |value| alloc_str_object(&value.to_string())),
        "escapechar" => config
            .escapechar
            .map_or_else(none, |value| alloc_str_object(&value.to_string())),
        "doublequote" => alloc_bool_object(config.doublequote),
        "skipinitialspace" => alloc_bool_object(config.skipinitialspace),
        "lineterminator" => alloc_str_object(&config.lineterminator),
        "quoting" => alloc_int_object(config.quoting),
        "strict" => alloc_bool_object(config.strict),
        _ => unsafe { abi::pon_raise_attribute_error(owner, intern(name)) },
    }
}

// ---------------------------------------------------------------------------
// Writer

unsafe extern "C" fn writer_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name_text) = (unsafe { unicode_text(untag(name)) }) else {
        return raise_type_error("attribute name must be str");
    };
    let Some(writer) = (unsafe { as_writer(object) }) else {
        return raise_type_error("_csv.writer receiver is invalid");
    };
    match name_text {
        "dialect" => alloc_dialect(writer.dialect.clone()),
        "writerow" => bound_method(object, name_text, writerow_method),
        "writerows" => bound_method(object, name_text, writerows_method),
        _ => unsafe { abi::pon_raise_attribute_error(object, intern(name_text)) },
    }
}

unsafe extern "C" fn writerow_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return abi::return_null_with_error("writerow() received a null argv pointer");
    };
    if args.len() != 2 {
        return raise_type_error(&format!("writerow() takes exactly one argument ({} given)", args.len().saturating_sub(1)));
    }
    let Some(writer) = (unsafe { as_writer(args[0]) }) else {
        return raise_type_error("writerow() receiver is invalid");
    };
    writerow(writer, args[1])
}

unsafe extern "C" fn writerows_method(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let Some(args) = (unsafe { arg_slice(argv, argc) }) else {
        return abi::return_null_with_error("writerows() received a null argv pointer");
    };
    if args.len() != 2 {
        return raise_type_error(&format!("writerows() takes exactly one argument ({} given)", args.len().saturating_sub(1)));
    }
    let Some(writer) = (unsafe { as_writer(args[0]) }) else {
        return raise_type_error("writerows() receiver is invalid");
    };
    let iter = unsafe { pon_get_iter(args[1], ptr::null_mut()) };
    if iter.is_null() {
        return ptr::null_mut();
    }
    loop {
        let row = unsafe { pon_iter_next(iter, ptr::null_mut()) };
        if row.is_null() {
            if pon_err_occurred() {
                if abi::exc::pending_exception_is("StopIteration") {
                    pon_err_clear();
                    break;
                }
                return ptr::null_mut();
            }
            break;
        }
        let result = writerow(writer, row);
        if result.is_null() {
            return ptr::null_mut();
        }
    }
    none()
}

fn writerow(writer: &mut CsvWriter, row: *mut PyObject) -> *mut PyObject {
    let fields = match crate::abi::seq::sequence_to_vec(row) {
        Ok(fields) => fields,
        Err(message) => {
            if pon_err_occurred() {
                return ptr::null_mut();
            }
            return raise_type_error(&message);
        }
    };
    let record = match render_record(&writer.dialect, &fields) {
        Ok(record) => record,
        Err(message) => return raise_csv_error(&message),
    };
    let text = alloc_str_object(&record);
    if text.is_null() {
        return ptr::null_mut();
    }
    let write = unsafe { abi::pon_get_attr(writer.file, intern("write"), ptr::null_mut()) };
    if write.is_null() {
        return ptr::null_mut();
    }
    let mut args = [text];
    unsafe { abi::pon_call(write, args.as_mut_ptr(), args.len()) }
}

fn render_record(dialect: &DialectConfig, fields: &[*mut PyObject]) -> Result<String, String> {
    let mut out = String::new();
    for (index, field) in fields.iter().copied().enumerate() {
        if index > 0 {
            out.push(dialect.delimiter);
        }
        append_field(&mut out, dialect, field, fields.len())?;
    }
    out.push_str(&dialect.lineterminator);
    Ok(out)
}

fn append_field(out: &mut String, dialect: &DialectConfig, field: *mut PyObject, row_len: usize) -> Result<(), String> {
    let field = untag(field);
    let field_is_none = is_none(field);
    let text = if field_is_none {
        String::new()
    } else {
        try_str_text(field).map_err(|()| "field string conversion failed".to_owned())?
    };
    let numeric = is_number(field);
    let string = is_string(field);

    let mut quoted = match dialect.quoting {
        QUOTE_ALL => true,
        QUOTE_NONNUMERIC => !numeric,
        QUOTE_STRINGS => string,
        QUOTE_NOTNULL => !field_is_none,
        QUOTE_NONE | QUOTE_MINIMAL => false,
        _ => false,
    };

    if text.is_empty() && row_len == 1 && !quoted {
        return Err("single empty field record must be quoted".to_owned());
    }
    if text.is_empty() && row_len == 1 {
        quoted = true;
    }

    let quotechar = dialect.quotechar;
    let mut body = String::with_capacity(text.len());
    for c in text.chars() {
        let is_quote = quotechar == Some(c);
        let is_delim = c == dialect.delimiter;
        let is_line = c == '\r' || c == '\n' || dialect.lineterminator.contains(c);
        let is_escape = dialect.escapechar == Some(c);

        if dialect.quoting == QUOTE_NONE {
            if is_quote || is_delim || is_line || is_escape {
                let Some(escape) = dialect.escapechar else {
                    return Err("need to escape, but no escapechar set".to_owned());
                };
                body.push(escape);
            }
            body.push(c);
            continue;
        }

        if is_escape {
            let Some(escape) = dialect.escapechar else {
                body.push(c);
                continue;
            };
            body.push(escape);
            body.push(c);
            continue;
        }

        if is_quote {
            if dialect.doublequote {
                quoted = true;
                let Some(quote) = quotechar else {
                    return Err("quotechar must be set if quoting enabled".to_owned());
                };
                body.push(quote);
                body.push(quote);
            } else {
                let Some(escape) = dialect.escapechar else {
                    return Err("need to escape, but no escapechar set".to_owned());
                };
                body.push(escape);
                body.push(c);
            }
        } else {
            if dialect.quoting == QUOTE_MINIMAL && (is_delim || is_line) {
                quoted = true;
            }
            body.push(c);
        }
    }

    if quoted {
        let Some(quote) = quotechar else {
            return Err("quotechar must be set if quoting enabled".to_owned());
        };
        out.push(quote);
        out.push_str(&body);
        out.push(quote);
    } else {
        out.push_str(&body);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Reader

unsafe extern "C" fn reader_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name_text) = (unsafe { unicode_text(untag(name)) }) else {
        return raise_type_error("attribute name must be str");
    };
    let Some(reader) = (unsafe { as_reader(object) }) else {
        return raise_type_error("_csv.reader receiver is invalid");
    };
    match name_text {
        "dialect" => alloc_dialect(reader.dialect.clone()),
        "line_num" => alloc_int_object(reader.line_num as i64),
        _ => unsafe { abi::pon_raise_attribute_error(object, intern(name_text)) },
    }
}

unsafe extern "C" fn reader_next_slot(object: *mut PyObject) -> *mut PyObject {
    let Some(reader) = (unsafe { as_reader(object) }) else {
        return raise_type_error("_csv.reader receiver is invalid");
    };
    match read_record(reader) {
        Ok(Some(fields)) => {
            let mut objects = Vec::with_capacity(fields.len());
            for field in fields {
                let object = alloc_str_object(&field);
                if object.is_null() {
                    return ptr::null_mut();
                }
                objects.push(object);
            }
            alloc_list(objects)
        }
        Ok(None) => unsafe { abi::pon_raise_stop_iteration(ptr::null_mut()) },
        Err(message) => raise_csv_error(&message),
    }
}

fn push_reader_char(field: &mut String, field_len: &mut i64, c: char) -> Result<(), String> {
    *field_len += 1;
    let limit = FIELD_SIZE_LIMIT.load(Ordering::Relaxed);
    if *field_len > limit {
        return Err(format!("field larger than field limit ({limit})"));
    }
    field.push(c);
    Ok(())
}

fn read_record(reader: &mut CsvReader) -> Result<Option<Vec<String>>, String> {
    let mut fields: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut field_len = 0_i64;
    let mut in_quotes = false;
    let mut after_quote = false;
    let mut at_field_start = true;
    let mut escaped = false;
    let mut record_has_data = false;
    let quotechar = reader.dialect.quotechar;
    let quotes_enabled = reader.dialect.quoting != QUOTE_NONE && quotechar.is_some();

    loop {
        let line_obj = unsafe { pon_iter_next(reader.iter, ptr::null_mut()) };
        if line_obj.is_null() {
            if pon_err_occurred() {
                if abi::exc::pending_exception_is("StopIteration") {
                    pon_err_clear();
                } else {
                    return Err("iteration raised an exception".to_owned());
                }
            }
            if in_quotes {
                if reader.dialect.strict {
                    return Err("unexpected end of data".to_owned());
                }
                fields.push(field);
                return Ok(Some(fields));
            }
            return Ok(None);
        }
        let Some(line) = (unsafe { unicode_text(untag(line_obj)) }) else {
            let type_name = unsafe { crate::types::dict::type_name(untag(line_obj)) }.unwrap_or("object");
            return Err(format!("iterator should return strings, not {type_name}"));
        };
        reader.line_num = reader.line_num.saturating_add(1);

        if line.is_empty() && !in_quotes && fields.is_empty() && field.is_empty() && at_field_start {
            return Ok(Some(Vec::new()));
        }

        let chars: Vec<char> = line.chars().collect();
        let mut escaped_line_break = false;
        let mut index = 0;
        while index < chars.len() {
            let c = chars[index];
            index += 1;

            if escaped {
                record_has_data = true;
                push_reader_char(&mut field, &mut field_len, c)?;
                escaped_line_break |= c == '\r' || c == '\n';
                escaped = false;
                at_field_start = false;
                continue;
            }

            if reader.dialect.escapechar == Some(c) {
                record_has_data = true;
                escaped = true;
                at_field_start = false;
                continue;
            }

            if in_quotes {
                record_has_data = true;
                if quotechar == Some(c) {
                    if reader.dialect.doublequote && index < chars.len() && quotechar == Some(chars[index]) {
                        index += 1;
                        push_reader_char(&mut field, &mut field_len, c)?;
                    } else {
                        in_quotes = false;
                        after_quote = true;
                    }
                } else {
                    push_reader_char(&mut field, &mut field_len, c)?;
                }
                continue;
            }

            if after_quote {
                if c == reader.dialect.delimiter {
                    fields.push(std::mem::take(&mut field));
                    field_len = 0;
                    after_quote = false;
                    at_field_start = true;
                    record_has_data = true;
                } else if c == '\r' || c == '\n' {
                    fields.push(field);
                    return Ok(Some(fields));
                } else if reader.dialect.skipinitialspace && c == ' ' {
                    // CPython tolerates padding after a closing quote in loose mode.
                } else if reader.dialect.strict {
                    return Err("',' expected after '".to_owned());
                } else {
                    record_has_data = true;
                    push_reader_char(&mut field, &mut field_len, c)?;
                    after_quote = false;
                    at_field_start = false;
                }
                continue;
            }

            if c == reader.dialect.delimiter {
                fields.push(std::mem::take(&mut field));
                field_len = 0;
                at_field_start = true;
                record_has_data = true;
                continue;
            }

            if c == '\r' || c == '\n' {
                if !record_has_data && fields.is_empty() && field.is_empty() {
                    return Ok(Some(Vec::new()));
                }
                fields.push(field);
                return Ok(Some(fields));
            }

            if at_field_start && reader.dialect.skipinitialspace && c == ' ' {
                continue;
            }

            if at_field_start && quotes_enabled && quotechar == Some(c) {
                in_quotes = true;
                at_field_start = false;
                record_has_data = true;
                continue;
            }

            record_has_data = true;
            at_field_start = false;
            push_reader_char(&mut field, &mut field_len, c)?;
        }

        if escaped_line_break {
            continue;
        }
        if !in_quotes {
            if !record_has_data && fields.is_empty() && field.is_empty() {
                return Ok(Some(Vec::new()));
            }
            fields.push(field);
            return Ok(Some(fields));
        }
    }
}
