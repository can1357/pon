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
//! - `get_theme(*, tty_file=None, force_color=False, force_no_color=False)` —
//!   returns one of two immortal `Theme` singletons (colored / no-color) whose
//!   `.argparse`/`.syntax`/`.traceback`/`.unittest` sections serve the vendored
//!   default-theme constants (empty strings in the no-color variant);
//! - `decolor(text)` — strips the vendored `ColorCodes` set;
//! - `ANSIColors` — the vendored escape-code table as an opaque singleton
//!   (`doctest` binds it at import and reads `.RED`/`.RESET` when rendering
//!   colored failure reports);
//! - `get_colors(colorize=False, *, file=None)` — the colored singleton or the
//!   all-empty-strings no-color one (the vendored `NoColors` instance);
//!   `doctest.DocTestRunner.summarize` calls it on every run;
//! - `COLORIZE = True` module flag.
//!
//! Keyword-only signatures bind through
//! `types::function::bind_native_keywords_for_name` rows.  Not served (out
//! of the consumed chains, loud `AttributeError` when reached): `NoColors`
//! as a module attribute, `get_colors`'s `ANSIColors()` construction
//! surface, `set_theme`, `default_theme`, `ThemeSection` dataclasses.

use std::{
	mem, ptr,
	sync::{LazyLock, Mutex},
};

use super::{builtins_mod::VARIADIC_ARITY, install_module};
use crate::{
	abi,
	intern::intern,
	object::{PyObject, PyObjectHeader, PyType},
	thread_state::pon_err_clear,
	types::exc::ExceptionKind,
};

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
	"\x1b[30m",
	BLUE,
	CYAN,
	GREEN,
	"\x1b[90m",
	MAGENTA,
	RED,
	"\x1b[37m",
	YELLOW,
	BOLD,
	"\x1b[1;30m",
	BOLD_BLUE,
	BOLD_CYAN,
	BOLD_GREEN,
	BOLD_MAGENTA,
	BOLD_RED,
	"\x1b[1;37m",
	BOLD_YELLOW,
	"\x1b[94m",
	"\x1b[96m",
	"\x1b[92m",
	"\x1b[95m",
	"\x1b[91m",
	"\x1b[97m",
	"\x1b[93m",
	"\x1b[40m",
	"\x1b[44m",
	"\x1b[46m",
	"\x1b[42m",
	"\x1b[45m",
	"\x1b[41m",
	"\x1b[47m",
	"\x1b[43m",
	"\x1b[100m",
	"\x1b[104m",
	"\x1b[106m",
	"\x1b[102m",
	"\x1b[105m",
	"\x1b[101m",
	"\x1b[107m",
	"\x1b[103m",
];

/// The complete vendored `ANSIColors` class table, verbatim (the named
/// subset above stays as consts because the theme field tables reference
/// them).  `GREY` and `INTENSE_BLACK` genuinely share `\x1b[90m` upstream.
const ANSI_COLORS: &[(&str, &str)] = &[
	("RESET", RESET),
	("BLACK", "\x1b[30m"),
	("BLUE", BLUE),
	("CYAN", CYAN),
	("GREEN", GREEN),
	("GREY", "\x1b[90m"),
	("MAGENTA", MAGENTA),
	("RED", RED),
	("WHITE", "\x1b[37m"),
	("YELLOW", YELLOW),
	("BOLD", BOLD),
	("BOLD_BLACK", "\x1b[1;30m"),
	("BOLD_BLUE", BOLD_BLUE),
	("BOLD_CYAN", BOLD_CYAN),
	("BOLD_GREEN", BOLD_GREEN),
	("BOLD_MAGENTA", BOLD_MAGENTA),
	("BOLD_RED", BOLD_RED),
	("BOLD_WHITE", "\x1b[1;37m"),
	("BOLD_YELLOW", BOLD_YELLOW),
	("INTENSE_BLACK", "\x1b[90m"),
	("INTENSE_BLUE", "\x1b[94m"),
	("INTENSE_CYAN", "\x1b[96m"),
	("INTENSE_GREEN", "\x1b[92m"),
	("INTENSE_MAGENTA", "\x1b[95m"),
	("INTENSE_RED", "\x1b[91m"),
	("INTENSE_WHITE", "\x1b[97m"),
	("INTENSE_YELLOW", "\x1b[93m"),
	("BACKGROUND_BLACK", "\x1b[40m"),
	("BACKGROUND_BLUE", "\x1b[44m"),
	("BACKGROUND_CYAN", "\x1b[46m"),
	("BACKGROUND_GREEN", "\x1b[42m"),
	("BACKGROUND_MAGENTA", "\x1b[45m"),
	("BACKGROUND_RED", "\x1b[41m"),
	("BACKGROUND_WHITE", "\x1b[47m"),
	("BACKGROUND_YELLOW", "\x1b[43m"),
	("INTENSE_BACKGROUND_BLACK", "\x1b[100m"),
	("INTENSE_BACKGROUND_BLUE", "\x1b[104m"),
	("INTENSE_BACKGROUND_CYAN", "\x1b[106m"),
	("INTENSE_BACKGROUND_GREEN", "\x1b[102m"),
	("INTENSE_BACKGROUND_MAGENTA", "\x1b[105m"),
	("INTENSE_BACKGROUND_RED", "\x1b[101m"),
	("INTENSE_BACKGROUND_WHITE", "\x1b[107m"),
	("INTENSE_BACKGROUND_YELLOW", "\x1b[103m"),
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

const UNITTEST_FIELDS: &[(&str, &str)] =
	&[("passed", GREEN), ("warn", YELLOW), ("fail", RED), ("fail_info", BOLD_RED), ("reset", RESET)];

#[derive(Clone, Copy)]
enum SectionKind {
	Argparse  = 0,
	Syntax    = 1,
	Traceback = 2,
	Unittest  = 3,
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
	kind:    SectionKind,
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
	let mut ty =
		PyType::new(abi::runtime_type_type().cast_const(), "Theme", mem::size_of::<PyTheme>());
	ty.tp_getattro = Some(theme_getattro);
	Box::into_raw(Box::new(ty)) as usize
});

/// `[no_color 4 sections, colored 4 sections]` singletons.
static SECTIONS: LazyLock<[usize; 8]> = LazyLock::new(|| {
	let mut sections = [0usize; 8];
	for colored in 0..2 {
		for kind in
			[SectionKind::Argparse, SectionKind::Syntax, SectionKind::Traceback, SectionKind::Unittest]
		{
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

static CURRENT_THEME: Mutex<usize> = Mutex::new(0);

fn theme_object(colored: bool) -> *mut PyObject {
	THEMES[usize::from(colored)] as *mut PyObject
}

fn current_theme_object() -> *mut PyObject {
	let mut slot = CURRENT_THEME
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());
	if *slot == 0 {
		*slot = theme_object(true) as usize;
	}
	*slot as *mut PyObject
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
		_ => {
			return abi::exc::raise_attribute_error_text(&format!(
				"'Theme' object has no attribute '{name}'"
			));
		},
	};
	(SECTIONS[usize::from(theme.colored) * 4 + kind as usize] as *mut PyObject).cast::<PyObject>()
}

// ---------------------------------------------------------------------------
// ANSIColors (immortal leaked boxes, string-only payloads)
//
// CPython's `ANSIColors` is a plain class of str constants and `NoColors`
// is an `ANSIColors()` instance with every field re-set to "".  pon serves
// both through one payload shape — the Theme pattern: a `colored` flag
// selecting the vendored code or the empty string.  The colored singleton
// doubles as the module's `ANSIColors` binding (attribute reads are the
// consumed surface; it is not callable) and as `get_colors`'s colored
// result.
// ---------------------------------------------------------------------------

#[repr(C)]
struct PyAnsiColors {
	ob_base: PyObjectHeader,
	colored: bool,
}

static ANSI_COLORS_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"ANSIColors",
		mem::size_of::<PyAnsiColors>(),
	);
	ty.tp_getattro = Some(ansi_colors_getattro);
	Box::into_raw(Box::new(ty)) as usize
});

/// `[no_color, colored]` ANSIColors singletons.
static ANSI_COLORS_OBJECTS: LazyLock<[usize; 2]> = LazyLock::new(|| {
	core::array::from_fn(|colored| {
		Box::into_raw(Box::new(PyAnsiColors {
			ob_base: PyObjectHeader::new(*ANSI_COLORS_TYPE as *mut PyType),
			colored: colored == 1,
		})) as usize
	})
});

fn ansi_colors_object(colored: bool) -> *mut PyObject {
	ANSI_COLORS_OBJECTS[usize::from(colored)] as *mut PyObject
}

unsafe extern "C" fn ansi_colors_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let Some(name) = attr_name_text(name) else {
		return abi::exc::raise_attribute_error_text("ANSIColors attribute name must be str");
	};
	// SAFETY: Receiver is one of the PyAnsiColors singletons.
	let colors = unsafe { &*object.cast::<PyAnsiColors>() };
	for &(field, code) in ANSI_COLORS {
		if field == name {
			return alloc_str_object(if colors.colored { code } else { "" });
		}
	}
	abi::exc::raise_attribute_error_text(&format!("'ANSIColors' object has no attribute '{name}'"))
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
		_ => {},
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
		},
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
	if colored {
		current_theme_object()
	} else {
		theme_object(false)
	}
}

unsafe extern "C" fn set_theme_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc != 1 || argv.is_null() {
		return abi::exc::raise_kind_error_text(
			ExceptionKind::TypeError,
			"set_theme() takes exactly one argument",
		);
	}
	let theme = crate::tag::untag_arg(unsafe { *argv });
	if theme.is_null() || unsafe { (*theme).ob_type } != *THEME_TYPE as *const PyType {
		return abi::exc::raise_kind_error_text(ExceptionKind::ValueError, "Expected Theme object");
	}
	*CURRENT_THEME
		.lock()
		.unwrap_or_else(|poison| poison.into_inner()) = theme as usize;
	crate::import::store_module_attr(intern("_colorize"), intern("_theme"), theme);
	none()
}

/// `get_colors(colorize=False, *, file=None)`; keyword binding delivers
/// `[colorize, file]`.  The colored singleton when forced or the
/// environment allows color, else the all-empty-strings no-color singleton
/// (the vendored `NoColors` instance).
unsafe extern "C" fn get_colors_entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = if argc == 0 || argv.is_null() {
		&[][..]
	} else {
		// SAFETY: The caller passed `argc` live argument slots.
		unsafe { std::slice::from_raw_parts(argv, argc) }
	};
	if args.len() > 2 {
		return abi::exc::raise_kind_error_text(
			crate::types::exc::ExceptionKind::TypeError,
			"get_colors() takes at most 1 positional argument",
		);
	}
	let colored = truthy(args.first().copied()) || can_colorize_value(args.get(1).copied());
	ansi_colors_object(colored)
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
	let Some(text) = (unsafe { crate::types::type_::unicode_text(crate::tag::untag_arg(text)) })
	else {
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

fn string_attr(name: &str, value: &str) -> Result<(u32, *mut PyObject), String> {
	let object = alloc_str_object(value);
	if object.is_null() {
		return Err(format!("failed to allocate _colorize.{name}"));
	}
	Ok((intern(name), object))
}

fn object_attr(name: &str, value: *mut PyObject) -> Result<(u32, *mut PyObject), String> {
	if value.is_null() {
		return Err(format!("failed to allocate _colorize.{name}"));
	}
	Ok((intern(name), value))
}

fn import_module(name: &str) -> Option<*mut PyObject> {
	let module = unsafe { crate::import::pon_import_name(intern(name), ptr::null(), 0, 0) };
	if module.is_null() {
		pon_err_clear();
		None
	} else {
		Some(module)
	}
}

fn import_attr(module_name: &str, attr: &str) -> Option<*mut PyObject> {
	let module = import_module(module_name)?;
	let value = unsafe { abi::pon_get_attr(module, intern(attr), ptr::null_mut()) };
	if value.is_null() {
		pon_err_clear();
		None
	} else {
		Some(value)
	}
}

fn color_codes_set() -> *mut PyObject {
	let mut items = Vec::with_capacity(COLOR_CODES.len());
	for &code in COLOR_CODES {
		let object = alloc_str_object(code);
		if object.is_null() {
			return ptr::null_mut();
		}
		items.push(object);
	}
	unsafe { abi::map::pon_build_set(items.as_mut_ptr(), items.len()) }
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
		("set_theme", set_theme_entry),
		("decolor", decolor_entry),
		("get_colors", get_colors_entry),
	] {
		// SAFETY: `entry` is a live builtin entry point.
		let function =
			unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(fn_name)) };
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
	attrs.push((intern("ANSIColors"), ansi_colors_object(true)));
	attrs.push((intern("NoColors"), ansi_colors_object(false)));
	attrs.push(object_attr("ColorCodes", color_codes_set())?);
	attrs.push((intern("Theme"), (*THEME_TYPE as *mut PyType).cast::<PyObject>()));
	attrs.push((intern("ThemeSection"), (*SECTION_TYPE as *mut PyType).cast::<PyObject>()));
	for class_name in ["Argparse", "Syntax", "Traceback", "Unittest"] {
		attrs.push((intern(class_name), (*SECTION_TYPE as *mut PyType).cast::<PyObject>()));
	}
	attrs.push((intern("default_theme"), theme_object(true)));
	attrs.push((intern("theme_no_color"), theme_object(false)));
	attrs.push((intern("_theme"), current_theme_object()));
	attrs.push(string_attr("attr", "INTENSE_BACKGROUND_YELLOW")?);
	attrs.push(string_attr("code", "\x1b[103m")?);
	attrs.push(object_attr("__annotate__", none())?);
	attrs.push(object_attr("__conditional_annotations__", unsafe {
		abi::map::pon_build_map(ptr::null_mut(), 0)
	})?);
	if let Some(module) = import_module("os") {
		attrs.push((intern("os"), module));
	}
	if let Some(module) = import_module("sys") {
		attrs.push((intern("sys"), module));
	}
	for &(module_name, attr_name) in &[
		("_collections_abc", "Callable"),
		("_collections_abc", "Iterator"),
		("_collections_abc", "Mapping"),
		("dataclasses", "dataclass"),
		("dataclasses", "field"),
		("dataclasses", "Field"),
	] {
		if let Some(value) = import_attr(module_name, attr_name) {
			attrs.push((intern(attr_name), value));
		}
	}
	install_module(name, attrs)
}
