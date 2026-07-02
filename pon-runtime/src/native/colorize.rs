//! Native `_colorize` seed (WS-IMPORT: `traceback` -> `unittest`).
//!
//! CPython 3.14's `Lib/_colorize.py` is pure Python but builds its themes
//! with `dataclasses`, which pulls `inspect` -> `annotationlib` -> `ast` ->
//! the C `_ast` module pon does not have.  This seed serves the surface the
//! import chain actually consumes with the exact color tables from the
//! vendored file:
//!
//! - `can_colorize(*, file=None)` — env checks (`PYTHON_COLORS`, `NO_COLOR`,
//!   `FORCE_COLOR`, `TERM=dumb`) then `file.isatty()`;
//! - `get_theme(*, tty_file=None, force_color=False, force_no_color=False)`
//!   — returns one of two immortal `Theme` singletons (colored / no-color)
//!   whose `.argparse`/`.syntax`/`.traceback`/`.unittest` sections serve the
//!   vendored default-theme constants (empty strings in the no-color
//!   variant);
//! - `decolor(text)` — strips the vendored `ColorCodes` set;
//! - `COLORIZE = True` module flag.
//!
//! Keyword-only signatures bind through
//! `types::function::bind_native_keywords_for_name` rows.  Not served (out
//! of the unittest chain, loud `AttributeError` when reached): `ANSIColors`,
//! `NoColors`, `get_colors`, `set_theme`, `default_theme`, `ThemeSection`
//! dataclasses.

use std::mem;
use std::ptr;
use std::sync::LazyLock;

use crate::abi;
use crate::intern::intern;
use crate::object::{PyObject, PyObjectHeader, PyType};
use crate::thread_state::pon_err_clear;

use super::builtins_mod::VARIADIC_ARITY;
use super::install_module;

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

// ---------------------------------------------------------------------------
// Color tables (verbatim from vendored `Lib/_colorize.py`)

const RESET: &str = "\x1b[0m";
const BLUE: &str = "\x1b[34m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const MAGENTA: &str = "\x1b[35m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const BOLD: &str = "\x1b[1m";
const BOLD_BLUE: &str = "\x1b[1;34m";
const BOLD_CYAN: &str = "\x1b[1;36m";
const BOLD_GREEN: &str = "\x1b[1;32m";
const BOLD_MAGENTA: &str = "\x1b[1;35m";
const BOLD_RED: &str = "\x1b[1;31m";
const BOLD_YELLOW: &str = "\x1b[1;33m";

/// Every `ANSIColors` code, for `decolor` (the vendored `ColorCodes` set).
const COLOR_CODES: &[&str] = &[
    RESET,
    "\x1b[30m", BLUE, CYAN, GREEN, "\x1b[90m", MAGENTA, RED, "\x1b[37m", YELLOW,
    BOLD, "\x1b[1;30m", BOLD_BLUE, BOLD_CYAN, BOLD_GREEN, BOLD_MAGENTA, BOLD_RED, "\x1b[1;37m", BOLD_YELLOW,
    "\x1b[94m", "\x1b[96m", "\x1b[92m", "\x1b[95m", "\x1b[91m", "\x1b[97m", "\x1b[93m",
    "\x1b[40m", "\x1b[44m", "\x1b[46m", "\x1b[42m", "\x1b[45m", "\x1b[41m", "\x1b[47m", "\x1b[43m",
    "\x1b[100m", "\x1b[104m", "\x1b[106m", "\x1b[102m", "\x1b[105m", "\x1b[101m", "\x1b[107m", "\x1b[103m",
];

const ARGPARSE_FIELDS: &[(&str, &str)] = &[
    ("usage", BOLD_BLUE),
    ("prog", BOLD_MAGENTA),
    ("prog_extra", MAGENTA),
    ("heading", BOLD_BLUE),
    ("summary_long_option", CYAN),
    ("summary_short_option", GREEN),
    ("summary_label", YELLOW),
    ("summary_action", GREEN),
    ("long_option", BOLD_CYAN),
    ("short_option", BOLD_GREEN),
    ("label", BOLD_YELLOW),
    ("action", BOLD_GREEN),
    ("reset", RESET),
];

const SYNTAX_FIELDS: &[(&str, &str)] = &[
    ("prompt", BOLD_MAGENTA),
    ("keyword", BOLD_BLUE),
    ("keyword_constant", BOLD_BLUE),
    ("builtin", CYAN),
    ("comment", RED),
    ("string", GREEN),
    ("number", YELLOW),
    ("op", RESET),
    ("definition", BOLD),
    ("soft_keyword", BOLD_BLUE),
    ("reset", RESET),
];

const TRACEBACK_FIELDS: &[(&str, &str)] = &[
    ("type", BOLD_MAGENTA),
    ("message", MAGENTA),
    ("filename", MAGENTA),
    ("line_no", MAGENTA),
    ("frame", MAGENTA),
    ("error_highlight", BOLD_RED),
    ("error_range", RED),
    ("reset", RESET),
];

const UNITTEST_FIELDS: &[(&str, &str)] = &[
    ("passed", GREEN),
    ("warn", YELLOW),
    ("fail", RED),
    ("fail_info", BOLD_RED),
    ("reset", RESET),
];

#[derive(Clone, Copy)]
enum SectionKind {
    Argparse = 0,
    Syntax = 1,
    Traceback = 2,
    Unittest = 3,
}

impl SectionKind {
    fn fields(self) -> &'static [(&'static str, &'static str)] {
        match self {
            SectionKind::Argparse => ARGPARSE_FIELDS,
            SectionKind::Syntax => SYNTAX_FIELDS,
            SectionKind::Traceback => TRACEBACK_FIELDS,
            SectionKind::Unittest => UNITTEST_FIELDS,
        }
    }
}

// ---------------------------------------------------------------------------
// Theme / ThemeSection objects (immortal leaked boxes, string-only payloads)

#[repr(C)]
struct PyThemeSection {
    ob_base: PyObjectHeader,
    kind: SectionKind,
    colored: bool,
}

#[repr(C)]
struct PyTheme {
    ob_base: PyObjectHeader,
    colored: bool,
}

static SECTION_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(
        abi::runtime_type_type().cast_const(),
        "ThemeSection",
        mem::size_of::<PyThemeSection>(),
    );
    ty.tp_getattro = Some(section_getattro);
    Box::into_raw(Box::new(ty)) as usize
});

static THEME_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(
        abi::runtime_type_type().cast_const(),
        "Theme",
        mem::size_of::<PyTheme>(),
    );
    ty.tp_getattro = Some(theme_getattro);
    Box::into_raw(Box::new(ty)) as usize
});

/// `[no_color 4 sections, colored 4 sections]` singletons.
static SECTIONS: LazyLock<[usize; 8]> = LazyLock::new(|| {
    let mut sections = [0usize; 8];
    for colored in 0..2 {
        for kind in [SectionKind::Argparse, SectionKind::Syntax, SectionKind::Traceback, SectionKind::Unittest] {
            let section = Box::into_raw(Box::new(PyThemeSection {
                ob_base: PyObjectHeader::new(*SECTION_TYPE as *mut PyType),
                kind,
                colored: colored == 1,
            }));
            sections[colored * 4 + kind as usize] = section as usize;
        }
    }
    sections
});

/// `[no_color, colored]` theme singletons.
static THEMES: LazyLock<[usize; 2]> = LazyLock::new(|| {
    core::array::from_fn(|colored| {
        Box::into_raw(Box::new(PyTheme {
            ob_base: PyObjectHeader::new(*THEME_TYPE as *mut PyType),
            colored: colored == 1,
        })) as usize
    })
});

fn theme_object(colored: bool) -> *mut PyObject {
    THEMES[usize::from(colored)] as *mut PyObject
}

fn alloc_str_object(text: &str) -> *mut PyObject {
    // SAFETY: Runtime allocation helper; NULL on failure with the error set.
    unsafe { abi::pon_const_str(text.as_ptr(), text.len()) }
}

fn attr_name_text<'a>(name: *mut PyObject) -> Option<&'a str> {
    // SAFETY: `unicode_text` type-checks its argument.
    unsafe { crate::types::type_::unicode_text(crate::tag::untag_arg(name)) }
}

unsafe extern "C" fn section_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name) = attr_name_text(name) else {
        return abi::exc::raise_attribute_error_text("ThemeSection attribute name must be str");
    };
    // SAFETY: Receiver is one of the PyThemeSection singletons.
    let section = unsafe { &*object.cast::<PyThemeSection>() };
    for &(field, color) in section.kind.fields() {
        if field == name {
            return alloc_str_object(if section.colored { color } else { "" });
        }
    }
    abi::exc::raise_attribute_error_text(&format!("'ThemeSection' object has no attribute '{name}'"))
}

unsafe extern "C" fn theme_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let Some(name) = attr_name_text(name) else {
        return abi::exc::raise_attribute_error_text("Theme attribute name must be str");
    };
    // SAFETY: Receiver is one of the PyTheme singletons.
    let theme = unsafe { &*object.cast::<PyTheme>() };
    let kind = match name {
        "argparse" => SectionKind::Argparse,
        "syntax" => SectionKind::Syntax,
        "traceback" => SectionKind::Traceback,
        "unittest" => SectionKind::Unittest,
        _ => return abi::exc::raise_attribute_error_text(&format!("'Theme' object has no attribute '{name}'")),
    };
    (SECTIONS[usize::from(theme.colored) * 4 + kind as usize] as *mut PyObject).cast::<PyObject>()
}

// ---------------------------------------------------------------------------
// Module functions

fn none() -> *mut PyObject {
    // SAFETY: Singleton accessor.
    unsafe { abi::pon_none() }
}

fn is_missing(object: Option<*mut PyObject>) -> bool {
    match object {
        None => true,
        Some(value) => value.is_null() || crate::tag::untag_arg(value) == none(),
    }
}

fn truthy(object: Option<*mut PyObject>) -> bool {
    match object {
        None => false,
        Some(value) if value.is_null() => false,
        // SAFETY: Truthiness helper follows the error-sentinel contract.
        Some(value) => (unsafe { abi::pon_is_true(value) }) == 1,
    }
}

/// `file.isatty()` with every failure (missing attr, call error, falsy)
/// mapped to `false`; pon runs embedded, so this is the common answer.
fn file_isatty(file: *mut PyObject) -> bool {
    // SAFETY: Attribute dispatch tolerates a null feedback cell.
    let isatty = unsafe { abi::pon_get_attr(file, intern("isatty"), ptr::null_mut()) };
    if isatty.is_null() {
        pon_err_clear();
        return false;
    }
    // SAFETY: Zero-argument call of a live callable.
    let result = unsafe { abi::pon_call(isatty, ptr::null_mut(), 0) };
    if result.is_null() {
        pon_err_clear();
        return false;
    }
    // SAFETY: Truthiness helper follows the error-sentinel contract.
    (unsafe { abi::pon_is_true(result) }) == 1
}

/// The vendored `can_colorize` env ladder, minus the win32 VT probe.
fn can_colorize_value(file: Option<*mut PyObject>) -> bool {
    match std::env::var("PYTHON_COLORS").ok().as_deref() {
        Some("0") => return false,
        Some("1") => return true,
        _ => {}
    }
    if std::env::var("NO_COLOR").is_ok_and(|value| !value.is_empty()) {
        return false;
    }
    if std::env::var("FORCE_COLOR").is_ok_and(|value| !value.is_empty()) {
        return true;
    }
    if std::env::var("TERM").ok().as_deref() == Some("dumb") {
        return false;
    }
    let file = match file {
        Some(value) if !is_missing(Some(value)) => crate::tag::untag_arg(value),
        _ => {
            // Default to sys.stdout, like the vendored module.
            let Some(sys_module) = crate::import::cached_module(intern("sys")) else {
                return false;
            };
            // SAFETY: Attribute dispatch tolerates a null feedback cell.
            let stdout = unsafe { abi::pon_get_attr(sys_module, intern("stdout"), ptr::null_mut()) };
            if stdout.is_null() {
                pon_err_clear();
                return false;
            }
            stdout
        }
    };
    file_isatty(file)
}

/// `can_colorize(*, file=None)`; keyword binding delivers `[file]`.
unsafe extern "C" fn can_colorize_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = if argc == 0 || argv.is_null() {
        &[][..]
    } else {
        // SAFETY: The caller passed `argc` live argument slots.
        unsafe { std::slice::from_raw_parts(argv, argc) }
    };
    if args.len() > 1 {
        return abi::exc::raise_kind_error_text(
            crate::types::exc::ExceptionKind::TypeError,
            "can_colorize() takes no positional arguments",
        );
    }
    // SAFETY: Bool constructor returns the singleton.
    unsafe { abi::number::pon_const_bool(i32::from(can_colorize_value(args.first().copied()))) }
}

/// `get_theme(*, tty_file=None, force_color=False, force_no_color=False)`;
/// keyword binding delivers `[tty_file, force_color, force_no_color]`.
unsafe extern "C" fn get_theme_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    let args = if argc == 0 || argv.is_null() {
        &[][..]
    } else {
        // SAFETY: The caller passed `argc` live argument slots.
        unsafe { std::slice::from_raw_parts(argv, argc) }
    };
    if args.len() > 3 {
        return abi::exc::raise_kind_error_text(
            crate::types::exc::ExceptionKind::TypeError,
            "get_theme() takes no positional arguments",
        );
    }
    let colored = if truthy(args.get(1).copied()) {
        true
    } else if truthy(args.get(2).copied()) {
        false
    } else {
        can_colorize_value(args.first().copied())
    };
    theme_object(colored)
}

/// `decolor(text)`: strips the vendored `ColorCodes` set.
unsafe extern "C" fn decolor_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc != 1 || argv.is_null() {
        return abi::exc::raise_kind_error_text(
            crate::types::exc::ExceptionKind::TypeError,
            "decolor() takes exactly one argument",
        );
    }
    // SAFETY: One live argument slot was just checked.
    let text = unsafe { *argv };
    // SAFETY: `unicode_text` type-checks its argument.
    let Some(text) = (unsafe { crate::types::type_::unicode_text(crate::tag::untag_arg(text)) }) else {
        return abi::exc::raise_kind_error_text(
            crate::types::exc::ExceptionKind::TypeError,
            "decolor() argument must be str",
        );
    };
    if !text.contains('\x1b') {
        return alloc_str_object(text);
    }
    let mut out = text.to_owned();
    for code in COLOR_CODES {
        if out.contains(code) {
            out = out.replace(code, "");
        }
    }
    alloc_str_object(&out)
}

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let name = "_colorize";
    // SAFETY: Runtime allocation helper; NULL is checked below.
    let name_obj = unsafe { abi::pon_const_str(name.as_ptr(), name.len()) };
    if name_obj.is_null() {
        return Err("failed to allocate _colorize.__name__".to_owned());
    }
    let mut attrs = vec![(intern("__name__"), name_obj)];
    for (fn_name, entry) in [
        ("can_colorize", can_colorize_entry as BuiltinFn),
        ("get_theme", get_theme_entry),
        ("decolor", decolor_entry),
    ] {
        // SAFETY: `entry` is a live builtin entry point.
        let function = unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(fn_name)) };
        if function.is_null() {
            return Err(format!("failed to allocate _colorize.{fn_name}"));
        }
        attrs.push((intern(fn_name), function));
    }
    // SAFETY: Bool constructor returns the singleton.
    let colorize_flag = unsafe { abi::number::pon_const_bool(1) };
    if colorize_flag.is_null() {
        return Err("failed to allocate _colorize.COLORIZE".to_owned());
    }
    attrs.push((intern("COLORIZE"), colorize_flag));
    install_module(name, attrs)
}
