//! Native `readline` compatibility module.
//!
//! Pon does not own the process' interactive line editor, but the stdlib uses
//! this module for history/completer bookkeeping at import time.  The functions
//! below implement that persistent Python-visible state (history file I/O,
//! completer hooks, delimiter configuration, and line buffer edits) without
//! pretending to drive a terminal reader.

use std::fs;
use std::path::PathBuf;
use std::ptr;
use std::sync::{LazyLock, Mutex};

use num_traits::ToPrimitive;

use crate::abi::{self, pon_const_int, pon_const_str, pon_make_function};
use crate::intern::intern;
use crate::object::PyObject;
use crate::types::exc::ExceptionKind;
use crate::types::type_::unicode_text;

use super::install_module;

const VARIADIC_ARITY: usize = crate::native::builtins_mod::VARIADIC_ARITY;
const DEFAULT_DELIMS: &str = " \t\n`~!@#$%^&*()-=+[{]}\\|;:'\",<>/?";

#[derive(Debug)]
struct ReadlineState {
    history: Vec<String>,
    history_length: i64,
    completer_delims: String,
    completer: usize,
    startup_hook: usize,
    pre_input_hook: usize,
    completion_display_matches_hook: usize,
    auto_history: bool,
    line_buffer: String,
    begidx: i64,
    endidx: i64,
    completion_type: i64,
}

static STATE: LazyLock<Mutex<ReadlineState>> = LazyLock::new(|| {
    Mutex::new(ReadlineState {
        history: Vec::new(),
        history_length: -1,
        completer_delims: DEFAULT_DELIMS.to_owned(),
        completer: 0,
        startup_hook: 0,
        pre_input_hook: 0,
        completion_display_matches_hook: 0,
        auto_history: true,
        line_buffer: String::new(),
        begidx: 0,
        endidx: 0,
        completion_type: 0,
    })
});

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let name = "readline";
    let mut attrs = vec![string_attr("__name__", name)?];
    attrs.push(string_attr("backend", "editline")?);
    attrs.push(string_attr("_READLINE_LIBRARY_VERSION", "EditLine wrapper")?);
    attrs.push(int_attr("_READLINE_VERSION", 1026)?);
    attrs.push(int_attr("_READLINE_RUNTIME_VERSION", 1026)?);
    for &(name, entry) in FUNCTIONS {
        attrs.push(function_attr(name, entry)?);
    }
    install_module(name, attrs)
}

const FUNCTIONS: &[(&str, unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject)] = &[
    ("add_history", readline_add_history),
    ("clear_history", readline_clear_history),
    ("get_begidx", readline_get_begidx),
    ("get_completer", readline_get_completer),
    ("get_completer_delims", readline_get_completer_delims),
    ("get_completion_type", readline_get_completion_type),
    ("get_current_history_length", readline_get_current_history_length),
    ("get_endidx", readline_get_endidx),
    ("get_history_item", readline_get_history_item),
    ("get_history_length", readline_get_history_length),
    ("get_line_buffer", readline_get_line_buffer),
    ("insert_text", readline_insert_text),
    ("parse_and_bind", readline_parse_and_bind),
    ("read_history_file", readline_read_history_file),
    ("read_init_file", readline_read_init_file),
    ("redisplay", readline_redisplay),
    ("remove_history_item", readline_remove_history_item),
    ("replace_history_item", readline_replace_history_item),
    ("set_auto_history", readline_set_auto_history),
    ("set_completer", readline_set_completer),
    ("set_completer_delims", readline_set_completer_delims),
    ("set_completion_display_matches_hook", readline_set_completion_display_matches_hook),
    ("set_history_length", readline_set_history_length),
    ("set_pre_input_hook", readline_set_pre_input_hook),
    ("set_startup_hook", readline_set_startup_hook),
    ("write_history_file", readline_write_history_file),
];

fn string_attr(name: &str, value: &str) -> Result<(u32, *mut PyObject), String> {
    let object = unsafe { pon_const_str(value.as_ptr(), value.len()) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate readline.{name}"))
}

fn int_attr(name: &str, value: i64) -> Result<(u32, *mut PyObject), String> {
    let object = unsafe { pon_const_int(value) };
    (!object.is_null())
        .then_some((intern(name), object))
        .ok_or_else(|| format!("failed to allocate readline.{name}"))
}

fn function_attr(
    name: &str,
    entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject,
) -> Result<(u32, *mut PyObject), String> {
    let function = unsafe { pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
    (!function.is_null())
        .then_some((intern(name), function))
        .ok_or_else(|| format!("failed to allocate readline.{name}"))
}

unsafe extern "C" fn readline_add_history(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match one_arg(argv, argc, "add_history") {
        Ok(args) => args,
        Err(error) => return error,
    };
    let text = match string_arg(args[0], "line") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let mut state = state();
    state.history.push(text);
    trim_history(&mut state);
    none()
}

unsafe extern "C" fn readline_clear_history(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if let Err(error) = no_args(argv, argc, "clear_history") {
        return error;
    }
    state().history.clear();
    none()
}

unsafe extern "C" fn readline_get_history_item(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match one_arg(argv, argc, "get_history_item") {
        Ok(args) => args,
        Err(error) => return error,
    };
    let index = match int_arg(args[0], "index") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let state = state();
    let Some(zero_based) = index.checked_sub(1).and_then(|value| usize::try_from(value).ok()) else {
        return none();
    };
    match state.history.get(zero_based) {
        Some(text) => str_object(text),
        None => none(),
    }
}

unsafe extern "C" fn readline_remove_history_item(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match one_arg(argv, argc, "remove_history_item") {
        Ok(args) => args,
        Err(error) => return error,
    };
    let index = match usize_arg(args[0], "pos") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let mut state = state();
    if index >= state.history.len() {
        return raise_value_error("No history item at position");
    }
    let removed = state.history.remove(index);
    str_object(&removed)
}

unsafe extern "C" fn readline_replace_history_item(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match unsafe { arg_slice(argv, argc) } {
        Some(args) if args.len() == 2 => args,
        _ => return raise_type_error(&format!("replace_history_item expected 2 arguments, got {argc}")),
    };
    let index = match usize_arg(args[0], "pos") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let text = match string_arg(args[1], "line") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let mut state = state();
    if index >= state.history.len() {
        return raise_value_error("No history item at position");
    }
    state.history[index] = text;
    none()
}

unsafe extern "C" fn readline_get_current_history_length(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if let Err(error) = no_args(argv, argc, "get_current_history_length") {
        return error;
    }
    unsafe { pon_const_int(state().history.len() as i64) }
}

unsafe extern "C" fn readline_get_history_length(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if let Err(error) = no_args(argv, argc, "get_history_length") {
        return error;
    }
    unsafe { pon_const_int(state().history_length) }
}

unsafe extern "C" fn readline_set_history_length(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match one_arg(argv, argc, "set_history_length") {
        Ok(args) => args,
        Err(error) => return error,
    };
    let length = match int_arg(args[0], "length") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let mut state = state();
    state.history_length = length;
    trim_history(&mut state);
    none()
}

unsafe extern "C" fn readline_read_history_file(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let path = match optional_path(argv, argc, "read_history_file") {
        Ok(path) => path,
        Err(error) => return error,
    };
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) => return raise_os_error(&error.to_string()),
    };
    let mut state = state();
    state.history = content.lines().map(str::to_owned).collect();
    trim_history(&mut state);
    none()
}

unsafe extern "C" fn readline_write_history_file(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let path = match optional_path(argv, argc, "write_history_file") {
        Ok(path) => path,
        Err(error) => return error,
    };
    let content = {
        let state = state();
        if state.history.is_empty() {
            String::new()
        } else {
            let mut content = state.history.join("\n");
            content.push('\n');
            content
        }
    };
    match fs::write(&path, content) {
        Ok(()) => none(),
        Err(error) => raise_os_error(&error.to_string()),
    }
}

unsafe extern "C" fn readline_get_completer_delims(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if let Err(error) = no_args(argv, argc, "get_completer_delims") {
        return error;
    }
    str_object(&state().completer_delims)
}

unsafe extern "C" fn readline_set_completer_delims(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match one_arg(argv, argc, "set_completer_delims") {
        Ok(args) => args,
        Err(error) => return error,
    };
    let delims = match string_arg(args[0], "delims") {
        Ok(value) => value,
        Err(error) => return error,
    };
    state().completer_delims = delims;
    none()
}

unsafe extern "C" fn readline_get_completer(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if let Err(error) = no_args(argv, argc, "get_completer") {
        return error;
    }
    object_or_none(state().completer)
}

unsafe extern "C" fn readline_set_completer(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    set_object_slot(argv, argc, "set_completer", |state, value| state.completer = value)
}

unsafe extern "C" fn readline_set_startup_hook(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    set_object_slot(argv, argc, "set_startup_hook", |state, value| state.startup_hook = value)
}

unsafe extern "C" fn readline_set_pre_input_hook(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    set_object_slot(argv, argc, "set_pre_input_hook", |state, value| state.pre_input_hook = value)
}

unsafe extern "C" fn readline_set_completion_display_matches_hook(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    set_object_slot(argv, argc, "set_completion_display_matches_hook", |state, value| {
        state.completion_display_matches_hook = value;
    })
}

unsafe extern "C" fn readline_set_auto_history(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match one_arg(argv, argc, "set_auto_history") {
        Ok(args) => args,
        Err(error) => return error,
    };
    let enabled = match unsafe { abi::pon_is_true(args[0]) } {
        0 => false,
        1 => true,
        _ => return ptr::null_mut(),
    };
    state().auto_history = enabled;
    none()
}

unsafe extern "C" fn readline_get_line_buffer(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if let Err(error) = no_args(argv, argc, "get_line_buffer") {
        return error;
    }
    str_object(&state().line_buffer)
}

unsafe extern "C" fn readline_insert_text(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match one_arg(argv, argc, "insert_text") {
        Ok(args) => args,
        Err(error) => return error,
    };
    let text = match string_arg(args[0], "text") {
        Ok(value) => value,
        Err(error) => return error,
    };
    let mut state = state();
    state.line_buffer.push_str(&text);
    state.endidx = state.line_buffer.chars().count() as i64;
    none()
}

unsafe extern "C" fn readline_get_begidx(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if let Err(error) = no_args(argv, argc, "get_begidx") {
        return error;
    }
    unsafe { pon_const_int(state().begidx) }
}

unsafe extern "C" fn readline_get_endidx(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if let Err(error) = no_args(argv, argc, "get_endidx") {
        return error;
    }
    unsafe { pon_const_int(state().endidx) }
}

unsafe extern "C" fn readline_get_completion_type(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if let Err(error) = no_args(argv, argc, "get_completion_type") {
        return error;
    }
    unsafe { pon_const_int(state().completion_type) }
}

unsafe extern "C" fn readline_parse_and_bind(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = match one_arg(argv, argc, "parse_and_bind") {
        Ok(args) => args,
        Err(error) => return error,
    };
    if let Err(error) = string_arg(args[0], "string") {
        return error;
    }
    none()
}

unsafe extern "C" fn readline_read_init_file(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    match unsafe { arg_slice(argv, argc) } {
        Some(args) if args.len() <= 1 => {
            if let Some(&path) = args.first() {
                if let Err(error) = string_arg(path, "filename") {
                    return error;
                }
            }
            none()
        }
        _ => raise_type_error(&format!("read_init_file expected at most 1 argument, got {argc}")),
    }
}

unsafe extern "C" fn readline_redisplay(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if let Err(error) = no_args(argv, argc, "redisplay") {
        return error;
    }
    none()
}

fn state() -> std::sync::MutexGuard<'static, ReadlineState> {
    STATE.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn trim_history(state: &mut ReadlineState) {
    if let Ok(limit) = usize::try_from(state.history_length) {
        if state.history.len() > limit {
            let drop = state.history.len() - limit;
            state.history.drain(0..drop);
        }
    }
}

fn set_object_slot(
    argv: *mut *mut PyObject,
    argc: usize,
    function: &str,
    set: impl FnOnce(&mut ReadlineState, usize),
) -> *mut PyObject {
    let args = match unsafe { arg_slice(argv, argc) } {
        Some(args) if args.len() == 1 => args,
        _ => return raise_type_error(&format!("{function} expected 1 argument, got {argc}")),
    };
    let value = if is_none(args[0]) { 0 } else { crate::tag::untag_arg(args[0]) as usize };
    set(&mut state(), value);
    none()
}

fn optional_path(argv: *mut *mut PyObject, argc: usize, function: &str) -> Result<PathBuf, *mut PyObject> {
    let args = unsafe { arg_slice(argv, argc) }.ok_or_else(|| raise_type_error("invalid argument vector"))?;
    if args.len() > 1 {
        return Err(raise_type_error(&format!("{function} expected at most 1 argument, got {argc}")));
    }
    if let Some(&object) = args.first() {
        return string_arg(object, "filename").map(PathBuf::from);
    }
    Ok(PathBuf::from(".history"))
}

fn one_arg<'a>(argv: *mut *mut PyObject, argc: usize, function: &str) -> Result<&'a [*mut PyObject], *mut PyObject> {
    match unsafe { arg_slice(argv, argc) } {
        Some(args) if args.len() == 1 => Ok(args),
        _ => Err(raise_type_error(&format!("{function} expected 1 argument, got {argc}"))),
    }
}

fn no_args(argv: *mut *mut PyObject, argc: usize, function: &str) -> Result<(), *mut PyObject> {
    match unsafe { arg_slice(argv, argc) } {
        Some(args) if args.is_empty() => Ok(()),
        _ => Err(raise_type_error(&format!("{function} expected no arguments, got {argc}"))),
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

fn object_or_none(value: usize) -> *mut PyObject {
    if value == 0 { none() } else { value as *mut PyObject }
}

fn is_none(object: *mut PyObject) -> bool {
    unsafe { crate::types::dict::type_name(crate::tag::untag_arg(object)) == Some("NoneType") }
}

fn none() -> *mut PyObject {
    unsafe { abi::pon_none() }
}

fn str_object(text: &str) -> *mut PyObject {
    unsafe { pon_const_str(text.as_ptr(), text.len()) }
}

fn string_arg(object: *mut PyObject, what: &str) -> Result<String, *mut PyObject> {
    let object = crate::tag::untag_arg(object);
    let Some(text) = (unsafe { unicode_text(object) }) else {
        return Err(raise_type_error(&format!("{what} must be str")));
    };
    Ok(text.to_owned())
}

fn usize_arg(object: *mut PyObject, what: &str) -> Result<usize, *mut PyObject> {
    let value = int_arg(object, what)?;
    usize::try_from(value).map_err(|_| raise_value_error(&format!("{what} is out of range")))
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

fn raise_os_error(message: &str) -> *mut PyObject {
    abi::exc::raise_kind_error_text(ExceptionKind::OSError, message)
}
