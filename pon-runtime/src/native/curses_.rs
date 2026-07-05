//! Native `_curses` and `_curses_panel` modules backed by host ncurses/panel.
//!
//! The surface is intentionally a real subset: constants mirror this host's
//! CPython/ncurses build, safe terminfo/tuning helpers call ncurses directly,
//! and window/panel constructors wrap real `WINDOW*`/`PANEL*` handles.  Large
//! interactive and wide-character corners remain absent rather than stubbed.

use core::{
	ffi::{c_char, c_int, c_short},
	ptr,
};
use std::{
	ffi::{CStr, CString},
	sync::{
		LazyLock, Mutex,
		atomic::{AtomicBool, Ordering},
	},
};

use num_traits::ToPrimitive;

use super::install_module;
use crate::{
	abi::{self, pon_const_int, pon_const_str, pon_make_function},
	intern::intern,
	object::{PyObject, PyObjectHeader, PyType},
	thread_state::{pon_err_clear, pon_err_message},
	types::{
		bytearray_ as bytearray_type, bytes_ as bytes_type,
		exc::ExceptionKind,
		memoryview as memoryview_type,
		type_::{self, unicode_text},
	},
};

const VARIADIC_ARITY: usize = crate::native::builtins_mod::VARIADIC_ARITY;
type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;
type CursesBool = u8;

#[repr(C)]
struct CWindow {
	_private: [u8; 0],
}

#[repr(C)]
struct CPanel {
	_private: [u8; 0],
}

#[repr(C)]
struct MouseEvent {
	id:     c_short,
	x:      c_int,
	y:      c_int,
	z:      c_int,
	bstate: libc::c_ulong,
}

#[repr(C)]
struct PyCursesWindow {
	ob_base: PyObjectHeader,
	window:  *mut CWindow,
	owned:   bool,
}

#[repr(C)]
struct PyCursesPanel {
	ob_base:       PyObjectHeader,
	panel:         *mut CPanel,
	window_object: *mut PyObject,
}

#[link(name = "ncurses")]
unsafe extern "C" {
	static mut LINES: c_int;
	static mut COLS: c_int;
	static mut COLORS: c_int;
	static mut COLOR_PAIRS: c_int;
	static mut TABSIZE: c_int;
	#[link_name = "newscr"]
	static mut NEWSCR: *mut CWindow;

	#[link_name = "initscr"]
	fn c_initscr() -> *mut CWindow;
	#[link_name = "endwin"]
	fn c_endwin() -> c_int;
	#[link_name = "isendwin"]
	fn c_isendwin() -> c_int;
	#[link_name = "newwin"]
	fn c_newwin(lines: c_int, cols: c_int, begin_y: c_int, begin_x: c_int) -> *mut CWindow;
	#[link_name = "newpad"]
	fn c_newpad(lines: c_int, cols: c_int) -> *mut CWindow;
	#[link_name = "wrefresh"]
	fn c_wrefresh(window: *mut CWindow) -> c_int;
	#[link_name = "wnoutrefresh"]
	fn c_wnoutrefresh(window: *mut CWindow) -> c_int;
	#[link_name = "werase"]
	fn c_werase(window: *mut CWindow) -> c_int;
	#[link_name = "wclear"]
	fn c_wclear(window: *mut CWindow) -> c_int;
	#[link_name = "wmove"]
	fn c_wmove(window: *mut CWindow, y: c_int, x: c_int) -> c_int;
	#[link_name = "waddnstr"]
	fn c_waddnstr(window: *mut CWindow, text: *const c_char, len: c_int) -> c_int;
	#[link_name = "keypad"]
	fn c_keypad(window: *mut CWindow, flag: c_int) -> c_int;
	#[link_name = "getmaxx"]
	fn c_getmaxx(window: *mut CWindow) -> c_int;
	#[link_name = "getmaxy"]
	fn c_getmaxy(window: *mut CWindow) -> c_int;

	#[link_name = "getcury"]
	fn c_getcury(window: *const CWindow) -> c_int;
	#[link_name = "getcurx"]
	fn c_getcurx(window: *const CWindow) -> c_int;
	#[link_name = "is_leaveok"]
	fn c_is_leaveok(window: *const CWindow) -> CursesBool;
	#[link_name = "leaveok"]
	fn c_leaveok(window: *mut CWindow, flag: CursesBool) -> c_int;
	#[link_name = "setupterm"]
	fn c_setupterm(term: *const c_char, fd: c_int, errret: *mut c_int) -> c_int;
	#[link_name = "tigetflag"]
	fn c_tigetflag(capname: *const c_char) -> c_int;
	#[link_name = "tigetnum"]
	fn c_tigetnum(capname: *const c_char) -> c_int;
	#[link_name = "tigetstr"]
	fn c_tigetstr(capname: *const c_char) -> *mut c_char;
	#[link_name = "putp"]
	fn c_putp(text: *const c_char) -> c_int;
	#[link_name = "tparm"]
	fn c_tparm(
		text: *const c_char,
		p1: libc::c_long,
		p2: libc::c_long,
		p3: libc::c_long,
		p4: libc::c_long,
		p5: libc::c_long,
		p6: libc::c_long,
		p7: libc::c_long,
		p8: libc::c_long,
		p9: libc::c_long,
	) -> *mut c_char;

	#[link_name = "cbreak"]
	fn c_cbreak() -> c_int;
	#[link_name = "nocbreak"]
	fn c_nocbreak() -> c_int;
	#[link_name = "echo"]
	fn c_echo() -> c_int;
	#[link_name = "noecho"]
	fn c_noecho() -> c_int;
	#[link_name = "raw"]
	fn c_raw() -> c_int;
	#[link_name = "noraw"]
	fn c_noraw() -> c_int;
	#[link_name = "nl"]
	fn c_nl() -> c_int;
	#[link_name = "nonl"]
	fn c_nonl() -> c_int;
	#[link_name = "beep"]
	fn c_beep() -> c_int;
	#[link_name = "flash"]
	fn c_flash() -> c_int;
	#[link_name = "napms"]
	fn c_napms(ms: c_int) -> c_int;
	#[link_name = "doupdate"]
	fn c_doupdate() -> c_int;
	#[link_name = "def_prog_mode"]
	fn c_def_prog_mode() -> c_int;
	#[link_name = "def_shell_mode"]
	fn c_def_shell_mode() -> c_int;
	#[link_name = "reset_prog_mode"]
	fn c_reset_prog_mode() -> c_int;
	#[link_name = "reset_shell_mode"]
	fn c_reset_shell_mode() -> c_int;
	#[link_name = "savetty"]
	fn c_savetty() -> c_int;
	#[link_name = "resetty"]
	fn c_resetty() -> c_int;
	#[link_name = "curs_set"]
	fn c_curs_set(visibility: c_int) -> c_int;
	#[link_name = "halfdelay"]
	fn c_halfdelay(tenths: c_int) -> c_int;
	#[link_name = "delay_output"]
	fn c_delay_output(ms: c_int) -> c_int;
	#[link_name = "flushinp"]
	fn c_flushinp() -> c_int;
	#[link_name = "filter"]
	fn c_filter();
	#[link_name = "qiflush"]
	fn c_qiflush();
	#[link_name = "noqiflush"]
	fn c_noqiflush();
	#[link_name = "intrflush"]
	fn c_intrflush(window: *mut CWindow, flag: c_int) -> c_int;
	#[link_name = "meta"]
	fn c_meta(window: *mut CWindow, flag: c_int) -> c_int;
	#[link_name = "mouseinterval"]
	fn c_mouseinterval(interval: c_int) -> c_int;
	#[link_name = "mousemask"]
	fn c_mousemask(newmask: u64, oldmask: *mut u64) -> u64;
	#[link_name = "typeahead"]
	fn c_typeahead(fd: c_int) -> c_int;
	#[link_name = "getmouse"]
	fn c_getmouse(event: *mut MouseEvent) -> c_int;
	#[link_name = "ungetmouse"]
	fn c_ungetmouse(event: *mut MouseEvent) -> c_int;
	#[link_name = "ungetch"]
	fn c_ungetch(ch: c_int) -> c_int;
	#[link_name = "unget_wch"]
	fn c_unget_wch(ch: libc::wchar_t) -> c_int;
	#[link_name = "use_env"]
	fn c_use_env(flag: c_int);

	#[link_name = "start_color"]
	fn c_start_color() -> c_int;
	#[link_name = "has_colors"]
	fn c_has_colors() -> c_int;
	#[link_name = "can_change_color"]
	fn c_can_change_color() -> c_int;
	#[link_name = "has_ic"]
	fn c_has_ic() -> c_int;
	#[link_name = "has_il"]
	fn c_has_il() -> c_int;
	#[link_name = "init_pair"]
	fn c_init_pair(pair: c_short, fg: c_short, bg: c_short) -> c_int;
	#[link_name = "pair_content"]
	fn c_pair_content(pair: c_short, fg: *mut c_short, bg: *mut c_short) -> c_int;
	#[link_name = "init_color"]
	fn c_init_color(color: c_short, red: c_short, green: c_short, blue: c_short) -> c_int;
	#[link_name = "color_content"]
	fn c_color_content(
		color: c_short,
		red: *mut c_short,
		green: *mut c_short,
		blue: *mut c_short,
	) -> c_int;
	#[link_name = "use_default_colors"]
	fn c_use_default_colors() -> c_int;
	#[link_name = "assume_default_colors"]
	fn c_assume_default_colors(fg: c_int, bg: c_int) -> c_int;

	#[link_name = "get_escdelay"]
	fn c_get_escdelay() -> c_int;
	#[link_name = "set_escdelay"]
	fn c_set_escdelay(ms: c_int) -> c_int;
	#[link_name = "set_tabsize"]
	fn c_set_tabsize(cols: c_int) -> c_int;
	#[link_name = "keyname"]
	fn c_keyname(ch: c_int) -> *mut c_char;
	#[link_name = "unctrl"]
	fn c_unctrl(ch: u32) -> *mut c_char;
	#[link_name = "erasechar"]
	fn c_erasechar() -> c_char;
	#[link_name = "killchar"]
	fn c_killchar() -> c_char;
	#[link_name = "has_key"]
	fn c_has_key(ch: c_int) -> c_int;
	#[link_name = "longname"]
	fn c_longname() -> *mut c_char;
	#[link_name = "termname"]
	fn c_termname() -> *mut c_char;
	#[link_name = "baudrate"]
	fn c_baudrate() -> c_int;
	#[link_name = "termattrs"]
	fn c_termattrs() -> u32;
	#[link_name = "resize_term"]
	fn c_resize_term(lines: c_int, cols: c_int) -> c_int;
	#[link_name = "resizeterm"]
	fn c_resizeterm(lines: c_int, cols: c_int) -> c_int;
	#[link_name = "is_term_resized"]
	fn c_is_term_resized(lines: c_int, cols: c_int) -> c_int;
}

#[link(name = "panel")]
unsafe extern "C" {
	#[link_name = "new_panel"]
	fn c_new_panel(window: *mut CWindow) -> *mut CPanel;
	#[link_name = "update_panels"]
	fn c_update_panels();
	#[link_name = "hide_panel"]
	fn c_hide_panel(panel: *mut CPanel) -> c_int;
	#[link_name = "show_panel"]
	fn c_show_panel(panel: *mut CPanel) -> c_int;
	#[link_name = "top_panel"]
	fn c_top_panel(panel: *mut CPanel) -> c_int;
	#[link_name = "bottom_panel"]
	fn c_bottom_panel(panel: *mut CPanel) -> c_int;
	#[link_name = "move_panel"]
	fn c_move_panel(panel: *mut CPanel, y: c_int, x: c_int) -> c_int;
	#[link_name = "replace_panel"]
	fn c_replace_panel(panel: *mut CPanel, window: *mut CWindow) -> c_int;
	#[link_name = "panel_hidden"]
	fn c_panel_hidden(panel: *const CPanel) -> c_int;
	#[link_name = "panel_window"]
	fn c_panel_window(panel: *const CPanel) -> *mut CWindow;
	#[link_name = "panel_above"]
	fn c_panel_above(panel: *const CPanel) -> *mut CPanel;
	#[link_name = "panel_below"]
	fn c_panel_below(panel: *const CPanel) -> *mut CPanel;
}

const CONSTANTS: &[(&str, i64)] = &[
	("ALL_MOUSE_EVENTS", 134217727i64),
	("A_ALTCHARSET", 4194304i64),
	("A_ATTRIBUTES", 4294967040i64),
	("A_BLINK", 524288i64),
	("A_BOLD", 2097152i64),
	("A_CHARTEXT", 255i64),
	("A_COLOR", 65280i64),
	("A_DIM", 1048576i64),
	("A_HORIZONTAL", 33554432i64),
	("A_INVIS", 8388608i64),
	("A_ITALIC", 2147483648i64),
	("A_LEFT", 67108864i64),
	("A_LOW", 134217728i64),
	("A_NORMAL", 0i64),
	("A_PROTECT", 16777216i64),
	("A_REVERSE", 262144i64),
	("A_RIGHT", 268435456i64),
	("A_STANDOUT", 65536i64),
	("A_TOP", 536870912i64),
	("A_UNDERLINE", 131072i64),
	("A_VERTICAL", 1073741824i64),
	("BUTTON1_CLICKED", 4i64),
	("BUTTON1_DOUBLE_CLICKED", 8i64),
	("BUTTON1_PRESSED", 2i64),
	("BUTTON1_RELEASED", 1i64),
	("BUTTON1_TRIPLE_CLICKED", 16i64),
	("BUTTON2_CLICKED", 256i64),
	("BUTTON2_DOUBLE_CLICKED", 512i64),
	("BUTTON2_PRESSED", 128i64),
	("BUTTON2_RELEASED", 64i64),
	("BUTTON2_TRIPLE_CLICKED", 1024i64),
	("BUTTON3_CLICKED", 16384i64),
	("BUTTON3_DOUBLE_CLICKED", 32768i64),
	("BUTTON3_PRESSED", 8192i64),
	("BUTTON3_RELEASED", 4096i64),
	("BUTTON3_TRIPLE_CLICKED", 65536i64),
	("BUTTON4_CLICKED", 1048576i64),
	("BUTTON4_DOUBLE_CLICKED", 2097152i64),
	("BUTTON4_PRESSED", 524288i64),
	("BUTTON4_RELEASED", 262144i64),
	("BUTTON4_TRIPLE_CLICKED", 4194304i64),
	("BUTTON_ALT", 67108864i64),
	("BUTTON_CTRL", 16777216i64),
	("BUTTON_SHIFT", 33554432i64),
	("COLOR_BLACK", 0i64),
	("COLOR_BLUE", 4i64),
	("COLOR_CYAN", 6i64),
	("COLOR_GREEN", 2i64),
	("COLOR_MAGENTA", 5i64),
	("COLOR_RED", 1i64),
	("COLOR_WHITE", 7i64),
	("COLOR_YELLOW", 3i64),
	("ERR", -1i64),
	("KEY_A1", 348i64),
	("KEY_A3", 349i64),
	("KEY_B2", 350i64),
	("KEY_BACKSPACE", 263i64),
	("KEY_BEG", 354i64),
	("KEY_BREAK", 257i64),
	("KEY_BTAB", 353i64),
	("KEY_C1", 351i64),
	("KEY_C3", 352i64),
	("KEY_CANCEL", 355i64),
	("KEY_CATAB", 342i64),
	("KEY_CLEAR", 333i64),
	("KEY_CLOSE", 356i64),
	("KEY_COMMAND", 357i64),
	("KEY_COPY", 358i64),
	("KEY_CREATE", 359i64),
	("KEY_CTAB", 341i64),
	("KEY_DC", 330i64),
	("KEY_DL", 328i64),
	("KEY_DOWN", 258i64),
	("KEY_EIC", 332i64),
	("KEY_END", 360i64),
	("KEY_ENTER", 343i64),
	("KEY_EOL", 335i64),
	("KEY_EOS", 334i64),
	("KEY_EXIT", 361i64),
	("KEY_F0", 264i64),
	("KEY_F1", 265i64),
	("KEY_F10", 274i64),
	("KEY_F11", 275i64),
	("KEY_F12", 276i64),
	("KEY_F13", 277i64),
	("KEY_F14", 278i64),
	("KEY_F15", 279i64),
	("KEY_F16", 280i64),
	("KEY_F17", 281i64),
	("KEY_F18", 282i64),
	("KEY_F19", 283i64),
	("KEY_F2", 266i64),
	("KEY_F20", 284i64),
	("KEY_F21", 285i64),
	("KEY_F22", 286i64),
	("KEY_F23", 287i64),
	("KEY_F24", 288i64),
	("KEY_F25", 289i64),
	("KEY_F26", 290i64),
	("KEY_F27", 291i64),
	("KEY_F28", 292i64),
	("KEY_F29", 293i64),
	("KEY_F3", 267i64),
	("KEY_F30", 294i64),
	("KEY_F31", 295i64),
	("KEY_F32", 296i64),
	("KEY_F33", 297i64),
	("KEY_F34", 298i64),
	("KEY_F35", 299i64),
	("KEY_F36", 300i64),
	("KEY_F37", 301i64),
	("KEY_F38", 302i64),
	("KEY_F39", 303i64),
	("KEY_F4", 268i64),
	("KEY_F40", 304i64),
	("KEY_F41", 305i64),
	("KEY_F42", 306i64),
	("KEY_F43", 307i64),
	("KEY_F44", 308i64),
	("KEY_F45", 309i64),
	("KEY_F46", 310i64),
	("KEY_F47", 311i64),
	("KEY_F48", 312i64),
	("KEY_F49", 313i64),
	("KEY_F5", 269i64),
	("KEY_F50", 314i64),
	("KEY_F51", 315i64),
	("KEY_F52", 316i64),
	("KEY_F53", 317i64),
	("KEY_F54", 318i64),
	("KEY_F55", 319i64),
	("KEY_F56", 320i64),
	("KEY_F57", 321i64),
	("KEY_F58", 322i64),
	("KEY_F59", 323i64),
	("KEY_F6", 270i64),
	("KEY_F60", 324i64),
	("KEY_F61", 325i64),
	("KEY_F62", 326i64),
	("KEY_F63", 327i64),
	("KEY_F7", 271i64),
	("KEY_F8", 272i64),
	("KEY_F9", 273i64),
	("KEY_FIND", 362i64),
	("KEY_HELP", 363i64),
	("KEY_HOME", 262i64),
	("KEY_IC", 331i64),
	("KEY_IL", 329i64),
	("KEY_LEFT", 260i64),
	("KEY_LL", 347i64),
	("KEY_MARK", 364i64),
	("KEY_MAX", 511i64),
	("KEY_MESSAGE", 365i64),
	("KEY_MIN", 257i64),
	("KEY_MOUSE", 409i64),
	("KEY_MOVE", 366i64),
	("KEY_NEXT", 367i64),
	("KEY_NPAGE", 338i64),
	("KEY_OPEN", 368i64),
	("KEY_OPTIONS", 369i64),
	("KEY_PPAGE", 339i64),
	("KEY_PREVIOUS", 370i64),
	("KEY_PRINT", 346i64),
	("KEY_REDO", 371i64),
	("KEY_REFERENCE", 372i64),
	("KEY_REFRESH", 373i64),
	("KEY_REPLACE", 374i64),
	("KEY_RESET", 345i64),
	("KEY_RESIZE", 410i64),
	("KEY_RESTART", 375i64),
	("KEY_RESUME", 376i64),
	("KEY_RIGHT", 261i64),
	("KEY_SAVE", 377i64),
	("KEY_SBEG", 378i64),
	("KEY_SCANCEL", 379i64),
	("KEY_SCOMMAND", 380i64),
	("KEY_SCOPY", 381i64),
	("KEY_SCREATE", 382i64),
	("KEY_SDC", 383i64),
	("KEY_SDL", 384i64),
	("KEY_SELECT", 385i64),
	("KEY_SEND", 386i64),
	("KEY_SEOL", 387i64),
	("KEY_SEXIT", 388i64),
	("KEY_SF", 336i64),
	("KEY_SFIND", 389i64),
	("KEY_SHELP", 390i64),
	("KEY_SHOME", 391i64),
	("KEY_SIC", 392i64),
	("KEY_SLEFT", 393i64),
	("KEY_SMESSAGE", 394i64),
	("KEY_SMOVE", 395i64),
	("KEY_SNEXT", 396i64),
	("KEY_SOPTIONS", 397i64),
	("KEY_SPREVIOUS", 398i64),
	("KEY_SPRINT", 399i64),
	("KEY_SR", 337i64),
	("KEY_SREDO", 400i64),
	("KEY_SREPLACE", 401i64),
	("KEY_SRESET", 344i64),
	("KEY_SRIGHT", 402i64),
	("KEY_SRSUME", 403i64),
	("KEY_SSAVE", 404i64),
	("KEY_SSUSPEND", 405i64),
	("KEY_STAB", 340i64),
	("KEY_SUNDO", 406i64),
	("KEY_SUSPEND", 407i64),
	("KEY_UNDO", 408i64),
	("KEY_UP", 259i64),
	("OK", 0i64),
	("REPORT_MOUSE_POSITION", 134217728i64),
];

static SCREEN_STARTED: AtomicBool = AtomicBool::new(false);
static SETUPTERM_DONE: AtomicBool = AtomicBool::new(false);
static WINDOW_REGISTRY: Mutex<Vec<usize>> = Mutex::new(Vec::new());
static PANEL_REGISTRY: Mutex<Vec<usize>> = Mutex::new(Vec::new());

static CURSES_ERROR: LazyLock<usize> = LazyLock::new(|| {
	exception_class("_curses", "error", "Exception").map_or(0, |class| class as usize)
});
static PANEL_ERROR: LazyLock<usize> = LazyLock::new(|| {
	exception_class("_curses_panel", "error", "Exception").map_or(0, |class| class as usize)
});

static WINDOW_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"_curses.window",
		core::mem::size_of::<PyCursesWindow>(),
	);
	ty.tp_base = abi::runtime_global(intern("object"))
		.map_or(ptr::null_mut(), |object| object.cast::<PyType>());
	ty.tp_getattro = Some(window_getattro);
	ty.tp_repr = Some(window_repr);
	set_type_module(&mut ty, "_curses");
	Box::into_raw(Box::new(ty)) as usize
});

static PANEL_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"_curses_panel.panel",
		core::mem::size_of::<PyCursesPanel>(),
	);
	ty.tp_base = abi::runtime_global(intern("object"))
		.map_or(ptr::null_mut(), |object| object.cast::<PyType>());
	ty.tp_getattro = Some(panel_getattro);
	ty.tp_repr = Some(panel_repr);
	set_type_module(&mut ty, "_curses_panel");
	Box::into_raw(Box::new(ty)) as usize
});

pub(super) fn make_curses_module() -> Result<*mut PyObject, String> {
	let mut attrs = Vec::with_capacity(CONSTANTS.len() + 56);
	attrs.push(bytes_attr("version", b"2.2")?);
	attrs.push(bytes_attr("__version__", b"2.2")?);
	let ncurses_version = ncurses_version_tuple();
	if ncurses_version.is_null() {
		return Err("failed to allocate _curses.ncurses_version".to_owned());
	}
	attrs.push((intern("ncurses_version"), ncurses_version));
	attrs.push((intern("error"), curses_error_class()));
	attrs.push((intern("window"), window_type().cast::<PyObject>()));
	for &(name, value) in CONSTANTS {
		attrs.push(int_attr(name, value)?);
	}
	for &(name, entry) in CURSES_FUNCTIONS {
		attrs.push(function_attr(name, entry)?);
	}
	install_module("_curses", attrs)
}

pub(super) fn make_curses_panel_module() -> Result<*mut PyObject, String> {
	install_module("_curses_panel", vec![
		str_attr("version", "2.1")?,
		str_attr("__version__", "2.1")?,
		(intern("error"), panel_error_class()),
		(intern("panel"), panel_type().cast::<PyObject>()),
		function_attr("new_panel", panel_new_panel)?,
		function_attr("update_panels", panel_update_panels)?,
		function_attr("bottom_panel", panel_bottom_panel)?,
		function_attr("top_panel", panel_top_panel)?,
	])
}

const CURSES_FUNCTIONS: &[(&str, BuiltinFn)] = &[
	("assume_default_colors", curses_assume_default_colors),
	("baudrate", curses_baudrate),
	("beep", curses_beep),
	("can_change_color", curses_can_change_color),
	("cbreak", curses_cbreak),
	("color_content", curses_color_content),
	("color_pair", curses_color_pair),
	("curs_set", curses_curs_set),
	("def_prog_mode", curses_def_prog_mode),
	("def_shell_mode", curses_def_shell_mode),
	("delay_output", curses_delay_output),
	("doupdate", curses_doupdate),
	("echo", curses_echo),
	("endwin", curses_endwin),
	("erasechar", curses_erasechar),
	("filter", curses_filter),
	("flash", curses_flash),
	("flushinp", curses_flushinp),
	("get_escdelay", curses_get_escdelay),
	("getmouse", curses_getmouse),
	("getsyx", curses_getsyx),
	("get_tabsize", curses_get_tabsize),
	("halfdelay", curses_halfdelay),
	("has_colors", curses_has_colors),
	("has_extended_color_support", curses_has_extended_color_support),
	("has_ic", curses_has_ic),
	("has_il", curses_has_il),
	("has_key", curses_has_key),
	("init_color", curses_init_color),
	("init_pair", curses_init_pair),
	("initscr", curses_initscr),
	("intrflush", curses_intrflush),
	("is_term_resized", curses_is_term_resized),
	("isendwin", curses_isendwin),
	("keyname", curses_keyname),
	("killchar", curses_killchar),
	("longname", curses_longname),
	("meta", curses_meta),
	("mouseinterval", curses_mouseinterval),
	("mousemask", curses_mousemask),
	("napms", curses_napms),
	("newpad", curses_newpad),
	("newwin", curses_newwin),
	("nl", curses_nl),
	("nocbreak", curses_nocbreak),
	("noecho", curses_noecho),
	("nonl", curses_nonl),
	("noqiflush", curses_noqiflush),
	("noraw", curses_noraw),
	("pair_content", curses_pair_content),
	("pair_number", curses_pair_number),
	("putp", curses_putp),
	("qiflush", curses_qiflush),
	("raw", curses_raw),
	("reset_prog_mode", curses_reset_prog_mode),
	("reset_shell_mode", curses_reset_shell_mode),
	("resetty", curses_resetty),
	("resize_term", curses_resize_term),
	("resizeterm", curses_resizeterm),
	("savetty", curses_savetty),
	("set_escdelay", curses_set_escdelay),
	("set_tabsize", curses_set_tabsize),
	("setupterm", curses_setupterm),
	("setsyx", curses_setsyx),
	("start_color", curses_start_color),
	("termattrs", curses_termattrs),
	("termname", curses_termname),
	("tigetflag", curses_tigetflag),
	("tigetnum", curses_tigetnum),
	("tigetstr", curses_tigetstr),
	("tparm", curses_tparm),
	("typeahead", curses_typeahead),
	("unctrl", curses_unctrl),
	("unget_wch", curses_unget_wch),
	("ungetch", curses_ungetch),
	("ungetmouse", curses_ungetmouse),
	("update_lines_cols", curses_update_lines_cols),
	("use_default_colors", curses_use_default_colors),
	("use_env", curses_use_env),
];

fn py_int(value: i64) -> *mut PyObject {
	unsafe { pon_const_int(value) }
}

fn py_bool(value: bool) -> *mut PyObject {
	unsafe { abi::pon_const_bool(c_int::from(value)) }
}

fn py_str(value: &str) -> *mut PyObject {
	unsafe { pon_const_str(value.as_ptr(), value.len()) }
}

fn py_bytes(value: &[u8]) -> *mut PyObject {
	unsafe { abi::str_::pon_const_bytes(value.as_ptr(), value.len()) }
}

fn none() -> *mut PyObject {
	unsafe { abi::pon_none() }
}

fn int_attr(name: &str, value: i64) -> Result<(u32, *mut PyObject), String> {
	let object = py_int(value);
	(!object.is_null())
		.then_some((intern(name), object))
		.ok_or_else(|| format!("failed to allocate _curses.{name}"))
}

fn str_attr(name: &str, value: &str) -> Result<(u32, *mut PyObject), String> {
	let object = py_str(value);
	(!object.is_null())
		.then_some((intern(name), object))
		.ok_or_else(|| format!("failed to allocate string attr {name}"))
}

fn bytes_attr(name: &str, value: &[u8]) -> Result<(u32, *mut PyObject), String> {
	let object = py_bytes(value);
	(!object.is_null())
		.then_some((intern(name), object))
		.ok_or_else(|| format!("failed to allocate bytes attr {name}"))
}

fn function_attr(name: &str, entry: BuiltinFn) -> Result<(u32, *mut PyObject), String> {
	let function = unsafe { pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
	(!function.is_null())
		.then_some((intern(name), function))
		.ok_or_else(|| format!("failed to allocate native function {name}"))
}

fn set_type_module(ty: &mut PyType, module: &str) {
	let namespace = crate::types::type_::new_namespace();
	if namespace.is_null() {
		return;
	}
	let module_object = py_str(module);
	if module_object.is_null() {
		return;
	}
	unsafe {
		(*namespace).set(intern("__module__"), module_object);
	}
	ty.tp_dict = namespace.cast::<PyObject>();
}

fn exception_class(module: &str, name: &str, base: &str) -> Result<*mut PyObject, String> {
	let base_class = unsafe { abi::pon_load_global(intern(base), ptr::null_mut()) };
	if base_class.is_null() {
		pon_err_clear();
		return Err(format!("builtin class '{base}' is not registered"));
	}
	let namespace = crate::types::type_::new_namespace();
	if namespace.is_null() {
		return Err(format!("failed to allocate {module}.{name} namespace"));
	}
	let module_object = py_str(module);
	if module_object.is_null() {
		return Err(format!("failed to allocate {module}.{name}.__module__"));
	}
	unsafe { (*namespace).set(intern("__module__"), module_object) };
	let class = unsafe {
		crate::types::type_::build_class_from_namespace(name, &[base_class], namespace, &[])
	};
	if class.is_null() {
		let detail = pon_err_message().unwrap_or_else(|| "unknown error".to_owned());
		pon_err_clear();
		return Err(format!("failed to create {module}.{name}: {detail}"));
	}
	unsafe {
		if (*class).ob_type.is_null() {
			(*class).ob_type = abi::runtime_type_type().cast_const();
		}
	}
	Ok(class)
}

fn raise_class(class_slot: &LazyLock<usize>, fallback: ExceptionKind, text: &str) -> *mut PyObject {
	let class = *LazyLock::force(class_slot);
	if class == 0 {
		return abi::exc::raise_kind_error_text(fallback, text);
	}
	let message = py_str(text);
	if message.is_null() {
		return ptr::null_mut();
	}
	let mut argv = [message];
	let instance = unsafe { abi::pon_call(class as *mut PyObject, argv.as_mut_ptr(), argv.len()) };
	if instance.is_null() {
		return ptr::null_mut();
	}
	unsafe { abi::exc::pon_raise(instance, ptr::null_mut()) }
}

fn curses_error_class() -> *mut PyObject {
	*LazyLock::force(&CURSES_ERROR) as *mut PyObject
}

fn panel_error_class() -> *mut PyObject {
	*LazyLock::force(&PANEL_ERROR) as *mut PyObject
}

fn raise_curses_error(message: &str) -> *mut PyObject {
	raise_class(&CURSES_ERROR, ExceptionKind::RuntimeError, message)
}

fn raise_panel_error(message: &str) -> *mut PyObject {
	raise_class(&PANEL_ERROR, ExceptionKind::RuntimeError, message)
}

fn type_error(message: &str) -> *mut PyObject {
	abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message)
}

fn value_error(message: &str) -> *mut PyObject {
	abi::exc::raise_kind_error_text(ExceptionKind::ValueError, message)
}

fn overflow_error(message: &str) -> *mut PyObject {
	abi::exc::raise_kind_error_text(ExceptionKind::OverflowError, message)
}

unsafe fn arg_slice<'a>(argv: *mut *mut PyObject, argc: usize) -> Option<&'a [*mut PyObject]> {
	if argc == 0 {
		Some(&[])
	} else if argv.is_null() {
		None
	} else {
		Some(unsafe { core::slice::from_raw_parts(argv, argc) })
	}
}

fn args_or_type_error<'a>(
	argv: *mut *mut PyObject,
	argc: usize,
	function: &str,
) -> Result<&'a [*mut PyObject], *mut PyObject> {
	unsafe { arg_slice(argv, argc) }
		.ok_or_else(|| type_error(&format!("{function}() received a null argument vector")))
}

fn untag(object: *mut PyObject) -> *mut PyObject {
	crate::tag::untag_arg(object)
}

fn is_none(object: *mut PyObject) -> bool {
	untag(object) == none()
}

fn type_name(object: *mut PyObject) -> &'static str {
	if crate::tag::is_small_int(object) {
		"int"
	} else {
		unsafe { crate::types::dict::type_name(untag(object)) }.unwrap_or("object")
	}
}

fn int_arg(object: *mut PyObject, name: &str) -> Result<i64, *mut PyObject> {
	let object = untag(object);
	let Some(value) = (unsafe { crate::types::int::to_bigint_including_bool(object) }) else {
		return Err(type_error(&format!("{name} must be an integer")));
	};
	value
		.to_i64()
		.ok_or_else(|| overflow_error(&format!("{name} is too large")))
}

fn c_int_arg(object: *mut PyObject, name: &str) -> Result<c_int, *mut PyObject> {
	c_int::try_from(int_arg(object, name)?)
		.map_err(|_| overflow_error(&format!("{name} is out of range")))
}

fn c_short_arg(object: *mut PyObject, name: &str) -> Result<c_short, *mut PyObject> {
	c_short::try_from(int_arg(object, name)?)
		.map_err(|_| overflow_error(&format!("{name} is out of range")))
}

fn bool_arg(object: *mut PyObject, name: &str) -> Result<c_int, *mut PyObject> {
	match unsafe { abi::pon_is_true(object) } {
		0 => Ok(0),
		1 => Ok(1),
		_ => Err(type_error(&format!("{name} must be truth-testable"))),
	}
}

fn str_arg(object: *mut PyObject, name: &str) -> Result<String, *mut PyObject> {
	let object = untag(object);
	unsafe { unicode_text(object) }
		.map(str::to_owned)
		.ok_or_else(|| type_error(&format!("{name} must be str, not '{}'", type_name(object))))
}

fn bytes_like<'a>(object: *mut PyObject) -> Option<&'a [u8]> {
	let object = untag(object);
	if object.is_null() {
		return None;
	}
	let ty = unsafe { (*object).ob_type };
	if bytes_type::is_bytes_type(ty) {
		return Some(unsafe { (*object.cast::<bytes_type::PyBytes>()).as_slice() });
	}
	if bytearray_type::is_bytearray_type(ty) {
		return Some(unsafe { (*object.cast::<bytearray_type::PyByteArray>()).as_slice() });
	}
	if memoryview_type::is_memoryview_type(ty) {
		let view = unsafe { &*object.cast::<memoryview_type::PyMemoryView>() };
		if view.released {
			return None;
		}
		return Some(unsafe { view.as_slice() });
	}
	None
}

fn bytes_or_text_arg(object: *mut PyObject, name: &str) -> Result<Vec<u8>, *mut PyObject> {
	let object = untag(object);
	if let Some(bytes) = bytes_like(object) {
		return Ok(bytes.to_vec());
	}
	if let Some(text) = unsafe { type_::unicode_text(object) } {
		return Ok(text.as_bytes().to_vec());
	}
	Err(type_error(&format!(
		"{name} must be a bytes-like object or str, not '{}'",
		type_name(object)
	)))
}

fn cstring_bytes_arg(object: *mut PyObject, name: &str) -> Result<CString, *mut PyObject> {
	CString::new(bytes_or_text_arg(object, name)?)
		.map_err(|_| value_error("embedded null character"))
}

fn sequence_items(object: *mut PyObject, name: &str) -> Result<Vec<*mut PyObject>, *mut PyObject> {
	let object = untag(object);
	if object.is_null() {
		return Err(type_error(&format!("{name} must be a sequence")));
	}
	match unsafe { crate::types::dict::type_name(object) } {
		Some("list") => {
			Ok(unsafe { (*object.cast::<crate::types::list::PyList>()).as_slice() }.to_vec())
		},
		Some("tuple") => {
			Ok(unsafe { (*object.cast::<crate::types::tuple::PyTuple>()).as_slice() }.to_vec())
		},
		_ => Err(type_error(&format!("{name} must be a sequence, not '{}'", type_name(object)))),
	}
}

fn cstring_arg(object: *mut PyObject, name: &str) -> Result<CString, *mut PyObject> {
	CString::new(str_arg(object, name)?).map_err(|_| value_error("embedded null character"))
}

fn check_started() -> Result<(), *mut PyObject> {
	if SCREEN_STARTED.load(Ordering::Acquire) {
		Ok(())
	} else {
		Err(raise_curses_error("must call initscr() first"))
	}
}

fn window_type() -> *mut PyType {
	*LazyLock::force(&WINDOW_TYPE) as *mut PyType
}

fn panel_type() -> *mut PyType {
	*LazyLock::force(&PANEL_TYPE) as *mut PyType
}

unsafe fn as_window<'a>(object: *mut PyObject) -> Option<&'a mut PyCursesWindow> {
	let object = untag(object);
	if object.is_null() || unsafe { (*object).ob_type } != window_type().cast_const() {
		return None;
	}
	Some(unsafe { &mut *object.cast::<PyCursesWindow>() })
}

unsafe fn as_panel<'a>(object: *mut PyObject) -> Option<&'a mut PyCursesPanel> {
	let object = untag(object);
	if object.is_null() || unsafe { (*object).ob_type } != panel_type().cast_const() {
		return None;
	}
	Some(unsafe { &mut *object.cast::<PyCursesPanel>() })
}

fn alloc_window(window: *mut CWindow, owned: bool) -> *mut PyObject {
	if window.is_null() {
		return raise_curses_error("curses window allocation failed");
	}
	let object = Box::into_raw(Box::new(PyCursesWindow {
		ob_base: PyObjectHeader::new(window_type()),
		window,
		owned,
	}));
	WINDOW_REGISTRY
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.push(object as usize);
	object.cast::<PyObject>()
}

fn alloc_panel(panel: *mut CPanel, window_object: *mut PyObject) -> *mut PyObject {
	if panel.is_null() {
		return raise_panel_error("panel allocation failed");
	}
	let object = Box::into_raw(Box::new(PyCursesPanel {
		ob_base: PyObjectHeader::new(panel_type()),
		panel,
		window_object,
	}));
	PANEL_REGISTRY
		.lock()
		.unwrap_or_else(|poison| poison.into_inner())
		.push(object as usize);
	object.cast::<PyObject>()
}

fn status_none(status: c_int, what: &str) -> *mut PyObject {
	if status == -1 {
		raise_curses_error(&format!("{what}() returned ERR"))
	} else {
		none()
	}
}

fn status_int(status: c_int, what: &str) -> *mut PyObject {
	if status == -1 {
		raise_curses_error(&format!("{what}() returned ERR"))
	} else {
		py_int(i64::from(status))
	}
}

fn status_panel_none(status: c_int, what: &str) -> *mut PyObject {
	if status == -1 {
		raise_panel_error(&format!("{what}() returned ERR"))
	} else {
		none()
	}
}

fn cstr_to_bytes(ptr: *const c_char) -> *mut PyObject {
	if ptr.is_null() {
		return none();
	}
	let bytes = unsafe { CStr::from_ptr(ptr) }.to_bytes();
	py_bytes(bytes)
}

fn ncurses_version_tuple() -> *mut PyObject {
	super::builtins_mod::alloc_tuple(vec![py_int(6), py_int(0), py_int(20150808)])
}

fn update_lines_cols_attrs() {
	let lines = unsafe { LINES };
	let cols = unsafe { COLS };
	let lines_obj = py_int(i64::from(lines));
	let cols_obj = py_int(i64::from(cols));
	if !lines_obj.is_null() {
		crate::import::store_module_attr(intern("_curses"), intern("LINES"), lines_obj);
	}
	if !cols_obj.is_null() {
		crate::import::store_module_attr(intern("_curses"), intern("COLS"), cols_obj);
	}
}

fn update_color_attrs() {
	let colors = unsafe { COLORS };
	let pairs = unsafe { COLOR_PAIRS };
	let colors_obj = py_int(i64::from(colors));
	let pairs_obj = py_int(i64::from(pairs));
	if !colors_obj.is_null() {
		crate::import::store_module_attr(intern("_curses"), intern("COLORS"), colors_obj);
	}
	if !pairs_obj.is_null() {
		crate::import::store_module_attr(intern("_curses"), intern("COLOR_PAIRS"), pairs_obj);
	}
}

unsafe fn no_args_status(
	argv: *mut *mut PyObject,
	argc: usize,
	name: &str,
	f: unsafe extern "C" fn() -> c_int,
) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	match args_or_type_error(argv, argc, name) {
		Ok([]) => status_none(unsafe { f() }, name),
		Ok(args) => type_error(&format!("{name}() takes no arguments ({} given)", args.len())),
		Err(error) => error,
	}
}

unsafe fn no_args_bool(
	argv: *mut *mut PyObject,
	argc: usize,
	name: &str,
	f: unsafe extern "C" fn() -> c_int,
) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	match args_or_type_error(argv, argc, name) {
		Ok([]) => py_bool(unsafe { f() } != 0),
		Ok(args) => type_error(&format!("{name}() takes no arguments ({} given)", args.len())),
		Err(error) => error,
	}
}

unsafe extern "C" fn curses_initscr(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	match args_or_type_error(argv, argc, "initscr") {
		Ok([]) => {},
		Ok(args) => {
			return type_error(&format!("initscr() takes no arguments ({} given)", args.len()));
		},
		Err(error) => return error,
	}
	let window = unsafe { c_initscr() };
	if window.is_null() {
		return raise_curses_error("initscr() returned NULL");
	}
	SCREEN_STARTED.store(true, Ordering::Release);
	update_lines_cols_attrs();
	alloc_window(window, false)
}

unsafe extern "C" fn curses_endwin(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	match args_or_type_error(argv, argc, "endwin") {
		Ok([]) => {},
		Ok(args) => {
			return type_error(&format!("endwin() takes no arguments ({} given)", args.len()));
		},
		Err(error) => return error,
	}
	status_none(unsafe { c_endwin() }, "endwin")
}

unsafe extern "C" fn curses_isendwin(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	match args_or_type_error(argv, argc, "isendwin") {
		Ok([]) => py_bool(unsafe { c_isendwin() } != 0),
		Ok(args) => type_error(&format!("isendwin() takes no arguments ({} given)", args.len())),
		Err(error) => error,
	}
}

unsafe extern "C" fn curses_setupterm(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "setupterm") {
		Ok(args) if args.len() <= 2 => args,
		Ok(args) => {
			return type_error(&format!(
				"setupterm() takes at most 2 arguments ({} given)",
				args.len()
			));
		},
		Err(error) => return error,
	};
	let term_storage = match args.first().copied() {
		Some(object) if !is_none(object) => Some(match cstring_arg(object, "term") {
			Ok(value) => value,
			Err(error) => return error,
		}),
		_ => None,
	};
	let fd = match args.get(1).copied() {
		Some(object) => match c_int_arg(object, "fd") {
			Ok(value) => value,
			Err(error) => return error,
		},
		None => -1,
	};
	let mut errret = 0;
	let term_ptr = term_storage
		.as_ref()
		.map_or(ptr::null(), |term| term.as_ptr());
	let status = unsafe { c_setupterm(term_ptr, fd, &mut errret) };
	if status == -1 || errret <= 0 {
		return raise_curses_error("setupterm() failed");
	}
	SETUPTERM_DONE.store(true, Ordering::Release);
	none()
}

unsafe extern "C" fn curses_get_escdelay(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	match args_or_type_error(argv, argc, "get_escdelay") {
		Ok([]) => py_int(i64::from(unsafe { c_get_escdelay() })),
		Ok(args) => type_error(&format!("get_escdelay() takes no arguments ({} given)", args.len())),
		Err(error) => error,
	}
}

unsafe extern "C" fn curses_set_escdelay(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "set_escdelay") {
		Ok([value]) => [*value],
		Ok(args) => {
			return type_error(&format!(
				"set_escdelay() takes exactly 1 argument ({} given)",
				args.len()
			));
		},
		Err(error) => return error,
	};
	let ms = match c_int_arg(args[0], "ms") {
		Ok(value) => value,
		Err(error) => return error,
	};
	status_none(unsafe { c_set_escdelay(ms) }, "set_escdelay")
}

unsafe extern "C" fn curses_get_tabsize(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	match args_or_type_error(argv, argc, "get_tabsize") {
		Ok([]) => py_int(i64::from(unsafe { TABSIZE })),
		Ok(args) => type_error(&format!("get_tabsize() takes no arguments ({} given)", args.len())),
		Err(error) => error,
	}
}

unsafe extern "C" fn curses_set_tabsize(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "set_tabsize") {
		Ok([value]) => [*value],
		Ok(args) => {
			return type_error(&format!(
				"set_tabsize() takes exactly 1 argument ({} given)",
				args.len()
			));
		},
		Err(error) => return error,
	};
	let cols = match c_int_arg(args[0], "cols") {
		Ok(value) => value,
		Err(error) => return error,
	};
	status_none(unsafe { c_set_tabsize(cols) }, "set_tabsize")
}

unsafe extern "C" fn curses_has_extended_color_support(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	match args_or_type_error(argv, argc, "has_extended_color_support") {
		Ok([]) => py_bool(false),
		Ok(args) => type_error(&format!(
			"has_extended_color_support() takes no arguments ({} given)",
			args.len()
		)),
		Err(error) => error,
	}
}

unsafe extern "C" fn curses_filter(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	match args_or_type_error(argv, argc, "filter") {
		Ok([]) => {
			unsafe { c_filter() };
			none()
		},
		Ok(args) => type_error(&format!("filter() takes no arguments ({} given)", args.len())),
		Err(error) => error,
	}
}

unsafe extern "C" fn curses_use_env(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "use_env") {
		Ok([flag]) => [*flag],
		Ok(args) => {
			return type_error(&format!("use_env() takes exactly 1 argument ({} given)", args.len()));
		},
		Err(error) => return error,
	};
	let flag = match bool_arg(args[0], "flag") {
		Ok(value) => value,
		Err(error) => return error,
	};
	unsafe { c_use_env(flag) };
	none()
}

unsafe extern "C" fn curses_qiflush(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	match args_or_type_error(argv, argc, "qiflush") {
		Ok([]) => {
			unsafe { c_qiflush() };
			none()
		},
		Ok(args) => type_error(&format!("qiflush() takes no arguments ({} given)", args.len())),
		Err(error) => error,
	}
}

unsafe extern "C" fn curses_noqiflush(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	match args_or_type_error(argv, argc, "noqiflush") {
		Ok([]) => {
			unsafe { c_noqiflush() };
			none()
		},
		Ok(args) => type_error(&format!("noqiflush() takes no arguments ({} given)", args.len())),
		Err(error) => error,
	}
}

macro_rules! noarg_status_fn {
	($rust:ident, $name:literal, $c:path) => {
		unsafe extern "C" fn $rust(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
			unsafe { no_args_status(argv, argc, $name, $c) }
		}
	};
}

macro_rules! noarg_bool_fn {
	($rust:ident, $name:literal, $c:path) => {
		unsafe extern "C" fn $rust(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
			unsafe { no_args_bool(argv, argc, $name, $c) }
		}
	};
}

noarg_status_fn!(curses_cbreak, "cbreak", c_cbreak);
noarg_status_fn!(curses_nocbreak, "nocbreak", c_nocbreak);
noarg_status_fn!(curses_echo, "echo", c_echo);
noarg_status_fn!(curses_noecho, "noecho", c_noecho);
noarg_status_fn!(curses_raw, "raw", c_raw);
noarg_status_fn!(curses_noraw, "noraw", c_noraw);
noarg_status_fn!(curses_nl, "nl", c_nl);
noarg_status_fn!(curses_nonl, "nonl", c_nonl);
noarg_status_fn!(curses_beep, "beep", c_beep);
noarg_status_fn!(curses_flash, "flash", c_flash);
noarg_status_fn!(curses_doupdate, "doupdate", c_doupdate);
noarg_status_fn!(curses_def_prog_mode, "def_prog_mode", c_def_prog_mode);
noarg_status_fn!(curses_def_shell_mode, "def_shell_mode", c_def_shell_mode);
noarg_status_fn!(curses_reset_prog_mode, "reset_prog_mode", c_reset_prog_mode);
noarg_status_fn!(curses_reset_shell_mode, "reset_shell_mode", c_reset_shell_mode);
noarg_status_fn!(curses_savetty, "savetty", c_savetty);
noarg_status_fn!(curses_resetty, "resetty", c_resetty);
noarg_status_fn!(curses_flushinp, "flushinp", c_flushinp);
noarg_bool_fn!(curses_has_colors, "has_colors", c_has_colors);
noarg_bool_fn!(curses_can_change_color, "can_change_color", c_can_change_color);
noarg_bool_fn!(curses_has_ic, "has_ic", c_has_ic);
noarg_bool_fn!(curses_has_il, "has_il", c_has_il);

unsafe extern "C" fn curses_start_color(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	match args_or_type_error(argv, argc, "start_color") {
		Ok([]) => {},
		Ok(args) => {
			return type_error(&format!("start_color() takes no arguments ({} given)", args.len()));
		},
		Err(error) => return error,
	}
	let result = status_none(unsafe { c_start_color() }, "start_color");
	if !result.is_null() {
		update_color_attrs();
	}
	result
}

unsafe extern "C" fn curses_one_int_status(
	argv: *mut *mut PyObject,
	argc: usize,
	name: &str,
	f: unsafe extern "C" fn(c_int) -> c_int,
) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	let args = match args_or_type_error(argv, argc, name) {
		Ok([value]) => [*value],
		Ok(args) => {
			return type_error(&format!("{name}() takes exactly 1 argument ({} given)", args.len()));
		},
		Err(error) => return error,
	};
	let value = match c_int_arg(args[0], name) {
		Ok(value) => value,
		Err(error) => return error,
	};
	status_none(unsafe { f(value) }, name)
}

unsafe extern "C" fn curses_napms(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	unsafe { curses_one_int_status(argv, argc, "napms", c_napms) }
}
unsafe extern "C" fn curses_halfdelay(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	unsafe { curses_one_int_status(argv, argc, "halfdelay", c_halfdelay) }
}
unsafe extern "C" fn curses_delay_output(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	unsafe { curses_one_int_status(argv, argc, "delay_output", c_delay_output) }
}
unsafe extern "C" fn curses_mouseinterval(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	unsafe { curses_one_int_status(argv, argc, "mouseinterval", c_mouseinterval) }
}
unsafe extern "C" fn curses_typeahead(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	unsafe { curses_one_int_status(argv, argc, "typeahead", c_typeahead) }
}

unsafe extern "C" fn curses_curs_set(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	let args = match args_or_type_error(argv, argc, "curs_set") {
		Ok([visibility]) => [*visibility],
		Ok(args) => {
			return type_error(&format!("curs_set() takes exactly 1 argument ({} given)", args.len()));
		},
		Err(error) => return error,
	};
	let visibility = match c_int_arg(args[0], "visibility") {
		Ok(value) => value,
		Err(error) => return error,
	};
	status_int(unsafe { c_curs_set(visibility) }, "curs_set")
}

unsafe extern "C" fn curses_intrflush(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	let args = match args_or_type_error(argv, argc, "intrflush") {
		Ok([flag]) => [*flag],
		Ok(args) => {
			return type_error(&format!(
				"intrflush() takes exactly 1 argument ({} given)",
				args.len()
			));
		},
		Err(error) => return error,
	};
	let flag = match bool_arg(args[0], "flag") {
		Ok(value) => value,
		Err(error) => return error,
	};
	status_none(unsafe { c_intrflush(ptr::null_mut(), flag) }, "intrflush")
}

unsafe extern "C" fn curses_meta(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	let args = match args_or_type_error(argv, argc, "meta") {
		Ok([flag]) => [*flag],
		Ok(args) => {
			return type_error(&format!("meta() takes exactly 1 argument ({} given)", args.len()));
		},
		Err(error) => return error,
	};
	let flag = match bool_arg(args[0], "flag") {
		Ok(value) => value,
		Err(error) => return error,
	};
	status_none(unsafe { c_meta(ptr::null_mut(), flag) }, "meta")
}

unsafe extern "C" fn curses_color_pair(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "color_pair") {
		Ok([pair]) => [*pair],
		Ok(args) => {
			return type_error(&format!(
				"color_pair() takes exactly 1 argument ({} given)",
				args.len()
			));
		},
		Err(error) => return error,
	};
	let pair = match int_arg(args[0], "pair_number") {
		Ok(value) => value,
		Err(error) => return error,
	};
	py_int((pair << 8) & 0x0000_ff00)
}

unsafe extern "C" fn curses_pair_number(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "pair_number") {
		Ok([attr]) => [*attr],
		Ok(args) => {
			return type_error(&format!(
				"pair_number() takes exactly 1 argument ({} given)",
				args.len()
			));
		},
		Err(error) => return error,
	};
	let attr = match int_arg(args[0], "attr") {
		Ok(value) => value,
		Err(error) => return error,
	};
	py_int((attr & 0x0000_ff00) >> 8)
}

unsafe extern "C" fn curses_init_pair(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	let args = match args_or_type_error(argv, argc, "init_pair") {
		Ok([pair, fg, bg]) => [*pair, *fg, *bg],
		Ok(args) => {
			return type_error(&format!(
				"init_pair() takes exactly 3 arguments ({} given)",
				args.len()
			));
		},
		Err(error) => return error,
	};
	let pair = match c_short_arg(args[0], "pair_number") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let fg = match c_short_arg(args[1], "fg") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let bg = match c_short_arg(args[2], "bg") {
		Ok(value) => value,
		Err(error) => return error,
	};
	status_none(unsafe { c_init_pair(pair, fg, bg) }, "init_pair")
}

unsafe extern "C" fn curses_pair_content(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	let args = match args_or_type_error(argv, argc, "pair_content") {
		Ok([pair]) => [*pair],
		Ok(args) => {
			return type_error(&format!(
				"pair_content() takes exactly 1 argument ({} given)",
				args.len()
			));
		},
		Err(error) => return error,
	};
	let pair = match c_short_arg(args[0], "pair_number") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let mut fg = 0;
	let mut bg = 0;
	if unsafe { c_pair_content(pair, &mut fg, &mut bg) } == -1 {
		return raise_curses_error("pair_content() returned ERR");
	}
	let items = vec![py_int(i64::from(fg)), py_int(i64::from(bg))];
	if items.iter().any(|item| item.is_null()) {
		return ptr::null_mut();
	}
	super::builtins_mod::alloc_tuple(items)
}

unsafe extern "C" fn curses_init_color(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	let args = match args_or_type_error(argv, argc, "init_color") {
		Ok([color, red, green, blue]) => [*color, *red, *green, *blue],
		Ok(args) => {
			return type_error(&format!(
				"init_color() takes exactly 4 arguments ({} given)",
				args.len()
			));
		},
		Err(error) => return error,
	};
	let color = match c_short_arg(args[0], "color_number") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let red = match c_short_arg(args[1], "red") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let green = match c_short_arg(args[2], "green") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let blue = match c_short_arg(args[3], "blue") {
		Ok(value) => value,
		Err(error) => return error,
	};
	status_none(unsafe { c_init_color(color, red, green, blue) }, "init_color")
}

unsafe extern "C" fn curses_color_content(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	let args = match args_or_type_error(argv, argc, "color_content") {
		Ok([color]) => [*color],
		Ok(args) => {
			return type_error(&format!(
				"color_content() takes exactly 1 argument ({} given)",
				args.len()
			));
		},
		Err(error) => return error,
	};
	let color = match c_short_arg(args[0], "color_number") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let mut red = 0;
	let mut green = 0;
	let mut blue = 0;
	if unsafe { c_color_content(color, &mut red, &mut green, &mut blue) } == -1 {
		return raise_curses_error("color_content() returned ERR");
	}
	let items = vec![py_int(i64::from(red)), py_int(i64::from(green)), py_int(i64::from(blue))];
	if items.iter().any(|item| item.is_null()) {
		return ptr::null_mut();
	}
	super::builtins_mod::alloc_tuple(items)
}

unsafe extern "C" fn curses_use_default_colors(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	match args_or_type_error(argv, argc, "use_default_colors") {
		Ok([]) => status_none(unsafe { c_use_default_colors() }, "use_default_colors"),
		Ok(args) => {
			type_error(&format!("use_default_colors() takes no arguments ({} given)", args.len()))
		},
		Err(error) => error,
	}
}

unsafe extern "C" fn curses_assume_default_colors(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	let args = match args_or_type_error(argv, argc, "assume_default_colors") {
		Ok([fg, bg]) => [*fg, *bg],
		Ok(args) => {
			return type_error(&format!(
				"assume_default_colors() takes exactly 2 arguments ({} given)",
				args.len()
			));
		},
		Err(error) => return error,
	};
	let fg = match c_int_arg(args[0], "fg") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let bg = match c_int_arg(args[1], "bg") {
		Ok(value) => value,
		Err(error) => return error,
	};
	status_none(unsafe { c_assume_default_colors(fg, bg) }, "assume_default_colors")
}

unsafe extern "C" fn curses_keyname(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	let args = match args_or_type_error(argv, argc, "keyname") {
		Ok([ch]) => [*ch],
		Ok(args) => {
			return type_error(&format!("keyname() takes exactly 1 argument ({} given)", args.len()));
		},
		Err(error) => return error,
	};
	let ch = match c_int_arg(args[0], "ch") {
		Ok(value) => value,
		Err(error) => return error,
	};
	cstr_to_bytes(unsafe { c_keyname(ch) })
}

unsafe extern "C" fn curses_unctrl(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	let args = match args_or_type_error(argv, argc, "unctrl") {
		Ok([ch]) => [*ch],
		Ok(args) => {
			return type_error(&format!("unctrl() takes exactly 1 argument ({} given)", args.len()));
		},
		Err(error) => return error,
	};
	let ch = match c_int_arg(args[0], "ch") {
		Ok(value) => value,
		Err(error) => return error,
	};
	cstr_to_bytes(unsafe { c_unctrl(ch as u32) })
}

unsafe extern "C" fn curses_erasechar(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	match args_or_type_error(argv, argc, "erasechar") {
		Ok([]) => py_bytes(&[unsafe { c_erasechar() } as u8]),
		Ok(args) => type_error(&format!("erasechar() takes no arguments ({} given)", args.len())),
		Err(error) => error,
	}
}

unsafe extern "C" fn curses_killchar(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	match args_or_type_error(argv, argc, "killchar") {
		Ok([]) => py_bytes(&[unsafe { c_killchar() } as u8]),
		Ok(args) => type_error(&format!("killchar() takes no arguments ({} given)", args.len())),
		Err(error) => error,
	}
}

unsafe extern "C" fn curses_getsyx(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	match args_or_type_error(argv, argc, "getsyx") {
		Ok([]) => {
			let window = unsafe { NEWSCR };
			let (y, x) = if window.is_null() {
				(-1, -1)
			} else if unsafe { c_is_leaveok(window.cast_const()) } != 0 {
				(-1, -1)
			} else {
				(unsafe { c_getcury(window.cast_const()) }, unsafe { c_getcurx(window.cast_const()) })
			};
			let items = vec![py_int(i64::from(y)), py_int(i64::from(x))];
			if items.iter().any(|item| item.is_null()) {
				return ptr::null_mut();
			}
			super::builtins_mod::alloc_tuple(items)
		},
		Ok(args) => type_error(&format!("getsyx() takes no arguments ({} given)", args.len())),
		Err(error) => error,
	}
}

unsafe extern "C" fn curses_setsyx(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	let args = match args_or_type_error(argv, argc, "setsyx") {
		Ok([y, x]) => [*y, *x],
		Ok(args) => {
			return type_error(&format!("setsyx() takes exactly 2 arguments ({} given)", args.len()));
		},
		Err(error) => return error,
	};
	let y = match c_int_arg(args[0], "y") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let x = match c_int_arg(args[1], "x") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let window = unsafe { NEWSCR };
	if window.is_null() {
		return raise_curses_error("setsyx() called before a screen exists");
	}
	let status = if y == -1 && x == -1 {
		unsafe { c_leaveok(window, 1) }
	} else {
		let leave_status = unsafe { c_leaveok(window, 0) };
		if leave_status == -1 {
			-1
		} else {
			unsafe { c_wmove(window, y, x) }
		}
	};
	status_none(status, "setsyx")
}

unsafe extern "C" fn curses_tparm(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "tparm") {
		Ok(args) if !args.is_empty() && args.len() <= 10 => args,
		Ok(args) => {
			return type_error(&format!(
				"tparm() expected a capability string and at most 9 parameters ({} given)",
				args.len()
			));
		},
		Err(error) => return error,
	};
	let cap = match cstring_bytes_arg(args[0], "str") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let mut params = [0 as libc::c_long; 9];
	for (index, param) in args[1..].iter().enumerate() {
		let value = match int_arg(*param, "parameter") {
			Ok(value) => value,
			Err(error) => return error,
		};
		params[index] = value as libc::c_long;
	}
	let expanded = unsafe {
		c_tparm(
			cap.as_ptr(),
			params[0],
			params[1],
			params[2],
			params[3],
			params[4],
			params[5],
			params[6],
			params[7],
			params[8],
		)
	};
	cstr_to_bytes(expanded)
}

unsafe extern "C" fn curses_ungetch(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "ungetch") {
		Ok([ch]) => [*ch],
		Ok(args) => {
			return type_error(&format!("ungetch() takes exactly 1 argument ({} given)", args.len()));
		},
		Err(error) => return error,
	};
	let ch = match c_int_arg(args[0], "ch") {
		Ok(value) => value,
		Err(error) => return error,
	};
	status_none(unsafe { c_ungetch(ch) }, "ungetch")
}

unsafe extern "C" fn curses_unget_wch(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "unget_wch") {
		Ok([ch]) => [*ch],
		Ok(args) => {
			return type_error(&format!(
				"unget_wch() takes exactly 1 argument ({} given)",
				args.len()
			));
		},
		Err(error) => return error,
	};
	let value = if let Ok(value) = int_arg(args[0], "ch") {
		value
	} else if let Some(text) = unsafe { unicode_text(untag(args[0])) } {
		let mut chars = text.chars();
		let Some(ch) = chars.next() else {
			return value_error("unget_wch() argument must not be empty");
		};
		if chars.next().is_some() {
			return value_error("unget_wch() argument must be a single character");
		}
		i64::from(u32::from(ch))
	} else {
		return type_error("unget_wch() argument must be an integer or one-character str");
	};
	let ch = match libc::wchar_t::try_from(value) {
		Ok(value) => value,
		Err(_) => return overflow_error("unget_wch() argument is out of range"),
	};
	status_none(unsafe { c_unget_wch(ch) }, "unget_wch")
}

unsafe extern "C" fn curses_getmouse(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	match args_or_type_error(argv, argc, "getmouse") {
		Ok([]) => {
			let mut event = MouseEvent { id: 0, x: 0, y: 0, z: 0, bstate: 0 };
			if unsafe { c_getmouse(&mut event) } == -1 {
				return raise_curses_error("getmouse() returned ERR");
			}
			let items = vec![
				py_int(i64::from(event.id)),
				py_int(i64::from(event.x)),
				py_int(i64::from(event.y)),
				py_int(i64::from(event.z)),
				py_int(event.bstate as i64),
			];
			if items.iter().any(|item| item.is_null()) {
				return ptr::null_mut();
			}
			super::builtins_mod::alloc_tuple(items)
		},
		Ok(args) => type_error(&format!("getmouse() takes no arguments ({} given)", args.len())),
		Err(error) => error,
	}
}

unsafe extern "C" fn curses_ungetmouse(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "ungetmouse") {
		Ok([event]) => [*event],
		Ok(args) => {
			return type_error(&format!(
				"ungetmouse() takes exactly 1 argument ({} given)",
				args.len()
			));
		},
		Err(error) => return error,
	};
	let items = match sequence_items(args[0], "event") {
		Ok(items) => items,
		Err(error) => return error,
	};
	let [id, x, y, z, bstate] = items.as_slice() else {
		return type_error(&format!("event must contain 5 values, got {}", items.len()));
	};
	let id = match c_short_arg(*id, "id") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let x = match c_int_arg(*x, "x") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let y = match c_int_arg(*y, "y") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let z = match c_int_arg(*z, "z") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let bstate = match int_arg(*bstate, "bstate") {
		Ok(value) => value as libc::c_ulong,
		Err(error) => return error,
	};
	let mut event = MouseEvent { id, x, y, z, bstate };
	status_none(unsafe { c_ungetmouse(&mut event) }, "ungetmouse")
}

unsafe extern "C" fn curses_has_key(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	let args = match args_or_type_error(argv, argc, "has_key") {
		Ok([ch]) => [*ch],
		Ok(args) => {
			return type_error(&format!("has_key() takes exactly 1 argument ({} given)", args.len()));
		},
		Err(error) => return error,
	};
	let ch = match c_int_arg(args[0], "ch") {
		Ok(value) => value,
		Err(error) => return error,
	};
	py_bool(unsafe { c_has_key(ch) } != 0)
}

unsafe extern "C" fn curses_longname(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	match args_or_type_error(argv, argc, "longname") {
		Ok([]) => cstr_to_bytes(unsafe { c_longname() }),
		Ok(args) => type_error(&format!("longname() takes no arguments ({} given)", args.len())),
		Err(error) => error,
	}
}

unsafe extern "C" fn curses_termname(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	match args_or_type_error(argv, argc, "termname") {
		Ok([]) => cstr_to_bytes(unsafe { c_termname() }),
		Ok(args) => type_error(&format!("termname() takes no arguments ({} given)", args.len())),
		Err(error) => error,
	}
}

unsafe extern "C" fn curses_baudrate(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	match args_or_type_error(argv, argc, "baudrate") {
		Ok([]) => py_int(i64::from(unsafe { c_baudrate() })),
		Ok(args) => type_error(&format!("baudrate() takes no arguments ({} given)", args.len())),
		Err(error) => error,
	}
}

unsafe extern "C" fn curses_termattrs(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	match args_or_type_error(argv, argc, "termattrs") {
		Ok([]) => py_int(i64::from(unsafe { c_termattrs() })),
		Ok(args) => type_error(&format!("termattrs() takes no arguments ({} given)", args.len())),
		Err(error) => error,
	}
}

unsafe extern "C" fn curses_tigetflag(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "tigetflag") {
		Ok([cap]) => [*cap],
		Ok(args) => {
			return type_error(&format!(
				"tigetflag() takes exactly 1 argument ({} given)",
				args.len()
			));
		},
		Err(error) => return error,
	};
	let cap = match cstring_arg(args[0], "capname") {
		Ok(value) => value,
		Err(error) => return error,
	};
	py_int(i64::from(unsafe { c_tigetflag(cap.as_ptr()) }))
}

unsafe extern "C" fn curses_tigetnum(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "tigetnum") {
		Ok([cap]) => [*cap],
		Ok(args) => {
			return type_error(&format!("tigetnum() takes exactly 1 argument ({} given)", args.len()));
		},
		Err(error) => return error,
	};
	let cap = match cstring_arg(args[0], "capname") {
		Ok(value) => value,
		Err(error) => return error,
	};
	py_int(i64::from(unsafe { c_tigetnum(cap.as_ptr()) }))
}

unsafe extern "C" fn curses_tigetstr(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "tigetstr") {
		Ok([cap]) => [*cap],
		Ok(args) => {
			return type_error(&format!("tigetstr() takes exactly 1 argument ({} given)", args.len()));
		},
		Err(error) => return error,
	};
	let cap = match cstring_arg(args[0], "capname") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let value = unsafe { c_tigetstr(cap.as_ptr()) };
	if value.is_null() || value as isize == -1 {
		none()
	} else {
		cstr_to_bytes(value)
	}
}

unsafe extern "C" fn curses_putp(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "putp") {
		Ok([text]) => [*text],
		Ok(args) => {
			return type_error(&format!("putp() takes exactly 1 argument ({} given)", args.len()));
		},
		Err(error) => return error,
	};
	let text = match cstring_arg(args[0], "text") {
		Ok(value) => value,
		Err(error) => return error,
	};
	status_none(unsafe { c_putp(text.as_ptr()) }, "putp")
}

unsafe extern "C" fn curses_newwin(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	let args = match args_or_type_error(argv, argc, "newwin") {
		Ok(args) if args.len() == 2 || args.len() == 4 => args,
		Ok(args) => {
			return type_error(&format!("newwin() takes 2 or 4 arguments ({} given)", args.len()));
		},
		Err(error) => return error,
	};
	let (lines, cols, y, x) = if args.len() == 2 {
		(0, 0, args[0], args[1])
	} else {
		(
			match c_int_arg(args[0], "nlines") {
				Ok(value) => value,
				Err(error) => return error,
			},
			match c_int_arg(args[1], "ncols") {
				Ok(value) => value,
				Err(error) => return error,
			},
			args[2],
			args[3],
		)
	};
	let y = match c_int_arg(y, "begin_y") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let x = match c_int_arg(x, "begin_x") {
		Ok(value) => value,
		Err(error) => return error,
	};
	alloc_window(unsafe { c_newwin(lines, cols, y, x) }, true)
}

unsafe extern "C" fn curses_newpad(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	let args = match args_or_type_error(argv, argc, "newpad") {
		Ok([lines, cols]) => [*lines, *cols],
		Ok(args) => {
			return type_error(&format!("newpad() takes exactly 2 arguments ({} given)", args.len()));
		},
		Err(error) => return error,
	};
	let lines = match c_int_arg(args[0], "nlines") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let cols = match c_int_arg(args[1], "ncols") {
		Ok(value) => value,
		Err(error) => return error,
	};
	alloc_window(unsafe { c_newpad(lines, cols) }, true)
}

unsafe extern "C" fn curses_update_lines_cols(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	match args_or_type_error(argv, argc, "update_lines_cols") {
		Ok([]) => {
			update_lines_cols_attrs();
			none()
		},
		Ok(args) => {
			type_error(&format!("update_lines_cols() takes no arguments ({} given)", args.len()))
		},
		Err(error) => error,
	}
}

unsafe extern "C" fn curses_resize_term(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	unsafe { resize_common(argv, argc, "resize_term", c_resize_term) }
}

unsafe extern "C" fn curses_resizeterm(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	unsafe { resize_common(argv, argc, "resizeterm", c_resizeterm) }
}

unsafe fn resize_common(
	argv: *mut *mut PyObject,
	argc: usize,
	name: &str,
	f: unsafe extern "C" fn(c_int, c_int) -> c_int,
) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	let args = match args_or_type_error(argv, argc, name) {
		Ok([lines, cols]) => [*lines, *cols],
		Ok(args) => {
			return type_error(&format!("{name}() takes exactly 2 arguments ({} given)", args.len()));
		},
		Err(error) => return error,
	};
	let lines = match c_int_arg(args[0], "nlines") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let cols = match c_int_arg(args[1], "ncols") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let result = status_none(unsafe { f(lines, cols) }, name);
	if !result.is_null() {
		update_lines_cols_attrs();
	}
	result
}

unsafe extern "C" fn curses_is_term_resized(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	let args = match args_or_type_error(argv, argc, "is_term_resized") {
		Ok([lines, cols]) => [*lines, *cols],
		Ok(args) => {
			return type_error(&format!(
				"is_term_resized() takes exactly 2 arguments ({} given)",
				args.len()
			));
		},
		Err(error) => return error,
	};
	let lines = match c_int_arg(args[0], "nlines") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let cols = match c_int_arg(args[1], "ncols") {
		Ok(value) => value,
		Err(error) => return error,
	};
	py_bool(unsafe { c_is_term_resized(lines, cols) } != 0)
}

unsafe extern "C" fn curses_mousemask(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	let args = match args_or_type_error(argv, argc, "mousemask") {
		Ok([mask]) => [*mask],
		Ok(args) => {
			return type_error(&format!(
				"mousemask() takes exactly 1 argument ({} given)",
				args.len()
			));
		},
		Err(error) => return error,
	};
	let mask = match int_arg(args[0], "newmask") {
		Ok(value) => value as u64,
		Err(error) => return error,
	};
	let mut oldmask = 0u64;
	let availmask = unsafe { c_mousemask(mask, &mut oldmask) };
	let items = vec![py_int(availmask as i64), py_int(oldmask as i64)];
	if items.iter().any(|item| item.is_null()) {
		return ptr::null_mut();
	}
	super::builtins_mod::alloc_tuple(items)
}

unsafe extern "C" fn window_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let Some(name_text) = (unsafe { unicode_text(untag(name)) }) else {
		return type_error("attribute name must be str");
	};
	let Some(_) = (unsafe { as_window(object) }) else {
		return type_error("window receiver is invalid");
	};
	match name_text {
		"addstr" => bound_method(object, name_text, window_addstr),
		"clear" => bound_method(object, name_text, window_clear),
		"erase" => bound_method(object, name_text, window_erase),
		"getmaxyx" => bound_method(object, name_text, window_getmaxyx),
		"keypad" => bound_method(object, name_text, window_keypad),
		"move" => bound_method(object, name_text, window_move),
		"noutrefresh" => bound_method(object, name_text, window_noutrefresh),
		"refresh" => bound_method(object, name_text, window_refresh),
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
	}
}

unsafe extern "C" fn window_repr(object: *mut PyObject) -> *mut PyObject {
	let Some(window) = (unsafe { as_window(object) }) else {
		return type_error("window receiver is invalid");
	};
	py_str(&format!("<_curses.window object at {:p}>", window.window))
}

fn bound_method(receiver: *mut PyObject, name: &str, entry: BuiltinFn) -> *mut PyObject {
	let function = unsafe { pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
	if function.is_null() {
		return ptr::null_mut();
	}
	match crate::types::method::new_bound_method(function, receiver) {
		Ok(method) => method.cast::<PyObject>(),
		Err(message) => type_error(&message),
	}
}

unsafe fn window_receiver<'a>(
	args: &'a [*mut PyObject],
	method: &str,
) -> Result<(&'a mut PyCursesWindow, &'a [*mut PyObject]), *mut PyObject> {
	let Some((&receiver, rest)) = args.split_first() else {
		return Err(type_error(&format!("{method}() requires a receiver")));
	};
	let Some(window) = (unsafe { as_window(receiver) }) else {
		return Err(type_error(&format!("{method}() receiver is invalid")));
	};
	if window.window.is_null() {
		return Err(raise_curses_error("window is closed"));
	}
	Ok((window, rest))
}

unsafe extern "C" fn window_keypad(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "keypad") {
		Ok(args) => args,
		Err(error) => return error,
	};
	let (window, rest) = match unsafe { window_receiver(args, "keypad") } {
		Ok(value) => value,
		Err(error) => return error,
	};
	let [flag] = rest else {
		return type_error(&format!("keypad() takes exactly 1 argument ({} given)", rest.len()));
	};
	let flag = match bool_arg(*flag, "flag") {
		Ok(value) => value,
		Err(error) => return error,
	};
	status_none(unsafe { c_keypad(window.window, flag) }, "keypad")
}

unsafe extern "C" fn window_refresh(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	window_noarg_status(argv, argc, "refresh", c_wrefresh)
}

unsafe extern "C" fn window_noutrefresh(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	window_noarg_status(argv, argc, "noutrefresh", c_wnoutrefresh)
}

unsafe extern "C" fn window_erase(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	window_noarg_status(argv, argc, "erase", c_werase)
}

unsafe extern "C" fn window_clear(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	window_noarg_status(argv, argc, "clear", c_wclear)
}

fn window_noarg_status(
	argv: *mut *mut PyObject,
	argc: usize,
	name: &str,
	f: unsafe extern "C" fn(*mut CWindow) -> c_int,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, name) {
		Ok(args) => args,
		Err(error) => return error,
	};
	let (window, rest) = match unsafe { window_receiver(args, name) } {
		Ok(value) => value,
		Err(error) => return error,
	};
	if !rest.is_empty() {
		return type_error(&format!("{name}() takes no arguments ({} given)", rest.len()));
	}
	status_none(unsafe { f(window.window) }, name)
}

unsafe extern "C" fn window_move(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "move") {
		Ok(args) => args,
		Err(error) => return error,
	};
	let (window, rest) = match unsafe { window_receiver(args, "move") } {
		Ok(value) => value,
		Err(error) => return error,
	};
	let [y, x] = rest else {
		return type_error(&format!("move() takes exactly 2 arguments ({} given)", rest.len()));
	};
	let y = match c_int_arg(*y, "y") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let x = match c_int_arg(*x, "x") {
		Ok(value) => value,
		Err(error) => return error,
	};
	status_none(unsafe { c_wmove(window.window, y, x) }, "move")
}

unsafe extern "C" fn window_addstr(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "addstr") {
		Ok(args) => args,
		Err(error) => return error,
	};
	let (window, rest) = match unsafe { window_receiver(args, "addstr") } {
		Ok(value) => value,
		Err(error) => return error,
	};
	let text_obj = match rest {
		[text] => *text,
		[y, x, text] => {
			let y = match c_int_arg(*y, "y") {
				Ok(value) => value,
				Err(error) => return error,
			};
			let x = match c_int_arg(*x, "x") {
				Ok(value) => value,
				Err(error) => return error,
			};
			if unsafe { c_wmove(window.window, y, x) } == -1 {
				return raise_curses_error("move() returned ERR");
			}
			*text
		},
		_ => return type_error(&format!("addstr() takes 1 or 3 arguments ({} given)", rest.len())),
	};
	let text = match cstring_arg(text_obj, "str") {
		Ok(value) => value,
		Err(error) => return error,
	};
	status_none(
		unsafe { c_waddnstr(window.window, text.as_ptr(), text.as_bytes().len() as c_int) },
		"addstr",
	)
}

unsafe extern "C" fn window_getmaxyx(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "getmaxyx") {
		Ok(args) => args,
		Err(error) => return error,
	};
	let (window, rest) = match unsafe { window_receiver(args, "getmaxyx") } {
		Ok(value) => value,
		Err(error) => return error,
	};
	if !rest.is_empty() {
		return type_error(&format!("getmaxyx() takes no arguments ({} given)", rest.len()));
	}
	let items = vec![
		py_int(i64::from(unsafe { c_getmaxy(window.window) })),
		py_int(i64::from(unsafe { c_getmaxx(window.window) })),
	];
	if items.iter().any(|item| item.is_null()) {
		return ptr::null_mut();
	}
	super::builtins_mod::alloc_tuple(items)
}

fn panel_object_for(raw: *mut CPanel) -> *mut PyObject {
	if raw.is_null() {
		return none();
	}
	let registry = PANEL_REGISTRY.lock().expect("panel registry poisoned");
	for &object in registry.iter() {
		let object = object as *mut PyObject;
		if unsafe { as_panel(object) }.is_some_and(|panel| panel.panel == raw) {
			return object;
		}
	}
	none()
}

unsafe extern "C" fn panel_top_panel(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	match args_or_type_error(argv, argc, "top_panel") {
		Ok([]) => panel_object_for(unsafe { c_panel_below(ptr::null()) }),
		Ok(args) => type_error(&format!("top_panel() takes no arguments ({} given)", args.len())),
		Err(error) => error,
	}
}

unsafe extern "C" fn panel_bottom_panel(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if let Err(error) = check_started() {
		return error;
	}
	match args_or_type_error(argv, argc, "bottom_panel") {
		Ok([]) => panel_object_for(unsafe { c_panel_above(ptr::null()) }),
		Ok(args) => type_error(&format!("bottom_panel() takes no arguments ({} given)", args.len())),
		Err(error) => error,
	}
}

unsafe extern "C" fn panel_new_panel(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "new_panel") {
		Ok([window]) => [*window],
		Ok(args) => {
			return type_error(&format!(
				"new_panel() takes exactly 1 argument ({} given)",
				args.len()
			));
		},
		Err(error) => return error,
	};
	let Some(window) = (unsafe { as_window(args[0]) }) else {
		return type_error("new_panel() argument must be a _curses.window");
	};
	if window.window.is_null() {
		return raise_panel_error("window is closed");
	}
	alloc_panel(unsafe { c_new_panel(window.window) }, args[0])
}

unsafe extern "C" fn panel_update_panels(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	match args_or_type_error(argv, argc, "update_panels") {
		Ok([]) => {
			unsafe { c_update_panels() };
			none()
		},
		Ok(args) => type_error(&format!("update_panels() takes no arguments ({} given)", args.len())),
		Err(error) => error,
	}
}

unsafe extern "C" fn panel_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let Some(name_text) = (unsafe { unicode_text(untag(name)) }) else {
		return type_error("attribute name must be str");
	};
	let Some(_) = (unsafe { as_panel(object) }) else {
		return type_error("panel receiver is invalid");
	};
	match name_text {
		"bottom" => bound_method(object, name_text, panel_bottom),
		"hidden" => bound_method(object, name_text, panel_hidden),
		"hide" => bound_method(object, name_text, panel_hide),
		"move" => bound_method(object, name_text, panel_move),
		"replace" => bound_method(object, name_text, panel_replace),
		"show" => bound_method(object, name_text, panel_show),
		"top" => bound_method(object, name_text, panel_top),
		"window" => bound_method(object, name_text, panel_window),
		_ => unsafe { abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
	}
}

unsafe extern "C" fn panel_repr(object: *mut PyObject) -> *mut PyObject {
	let Some(panel) = (unsafe { as_panel(object) }) else {
		return type_error("panel receiver is invalid");
	};
	py_str(&format!("<_curses_panel.panel object at {:p}>", panel.panel))
}

unsafe fn panel_receiver<'a>(
	args: &'a [*mut PyObject],
	method: &str,
) -> Result<(&'a mut PyCursesPanel, &'a [*mut PyObject]), *mut PyObject> {
	let Some((&receiver, rest)) = args.split_first() else {
		return Err(type_error(&format!("{method}() requires a receiver")));
	};
	let Some(panel) = (unsafe { as_panel(receiver) }) else {
		return Err(type_error(&format!("{method}() receiver is invalid")));
	};
	if panel.panel.is_null() {
		return Err(raise_panel_error("panel is closed"));
	}
	Ok((panel, rest))
}

fn panel_noarg_status(
	argv: *mut *mut PyObject,
	argc: usize,
	name: &str,
	f: unsafe extern "C" fn(*mut CPanel) -> c_int,
) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, name) {
		Ok(args) => args,
		Err(error) => return error,
	};
	let (panel, rest) = match unsafe { panel_receiver(args, name) } {
		Ok(value) => value,
		Err(error) => return error,
	};
	if !rest.is_empty() {
		return type_error(&format!("{name}() takes no arguments ({} given)", rest.len()));
	}
	status_panel_none(unsafe { f(panel.panel) }, name)
}

unsafe extern "C" fn panel_hide(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	panel_noarg_status(argv, argc, "hide", c_hide_panel)
}
unsafe extern "C" fn panel_show(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	panel_noarg_status(argv, argc, "show", c_show_panel)
}
unsafe extern "C" fn panel_top(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	panel_noarg_status(argv, argc, "top", c_top_panel)
}
unsafe extern "C" fn panel_bottom(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	panel_noarg_status(argv, argc, "bottom", c_bottom_panel)
}

unsafe extern "C" fn panel_hidden(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "hidden") {
		Ok(args) => args,
		Err(error) => return error,
	};
	let (panel, rest) = match unsafe { panel_receiver(args, "hidden") } {
		Ok(value) => value,
		Err(error) => return error,
	};
	if !rest.is_empty() {
		return type_error(&format!("hidden() takes no arguments ({} given)", rest.len()));
	}
	py_bool(unsafe { c_panel_hidden(panel.panel) } != 0)
}

unsafe extern "C" fn panel_move(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "move") {
		Ok(args) => args,
		Err(error) => return error,
	};
	let (panel, rest) = match unsafe { panel_receiver(args, "move") } {
		Ok(value) => value,
		Err(error) => return error,
	};
	let [y, x] = rest else {
		return type_error(&format!("move() takes exactly 2 arguments ({} given)", rest.len()));
	};
	let y = match c_int_arg(*y, "y") {
		Ok(value) => value,
		Err(error) => return error,
	};
	let x = match c_int_arg(*x, "x") {
		Ok(value) => value,
		Err(error) => return error,
	};
	status_panel_none(unsafe { c_move_panel(panel.panel, y, x) }, "move")
}

unsafe extern "C" fn panel_replace(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "replace") {
		Ok(args) => args,
		Err(error) => return error,
	};
	let (panel, rest) = match unsafe { panel_receiver(args, "replace") } {
		Ok(value) => value,
		Err(error) => return error,
	};
	let [window_obj] = rest else {
		return type_error(&format!("replace() takes exactly 1 argument ({} given)", rest.len()));
	};
	let Some(window) = (unsafe { as_window(*window_obj) }) else {
		return type_error("replace() argument must be a _curses.window");
	};
	if window.window.is_null() {
		return raise_panel_error("window is closed");
	}
	let status = unsafe { c_replace_panel(panel.panel, window.window) };
	if status != -1 {
		panel.window_object = *window_obj;
	}
	status_panel_none(status, "replace")
}

unsafe extern "C" fn panel_window(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match args_or_type_error(argv, argc, "window") {
		Ok(args) => args,
		Err(error) => return error,
	};
	let (panel, rest) = match unsafe { panel_receiver(args, "window") } {
		Ok(value) => value,
		Err(error) => return error,
	};
	if !rest.is_empty() {
		return type_error(&format!("window() takes no arguments ({} given)", rest.len()));
	}
	if !panel.window_object.is_null() {
		return panel.window_object;
	}
	alloc_window(unsafe { c_panel_window(panel.panel) }, false)
}
