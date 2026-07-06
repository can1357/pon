//! Curated native stdlib modules (HANDOFF Track L) and their lookup registry.
//!
//! Adding a native module (frozen J0.4 contract — see
//! `plans/pon-pin-J04-stdlib-registry.md`):
//! 1. add ONE `mod <file>;` declaration below, keeping the list sorted;
//! 2. insert ONE `("<python name>", <file>::make_module)` row into
//!    [`NATIVE_MODULES`], keeping the table sorted by module name.
//!
//! Existing rows are never edited or reordered. Eager startup registration is
//! frozen to [`EAGER_MODULES`]; everything else imports lazily on first use.

pub mod builtins_batch;
pub mod builtins_mod;
mod installed;

pub(crate) use crate::import::install_module;
use crate::object::PyObject;

mod array;
mod ast_;
pub use ast_::{AstNode, AstValue, NodeSpan, build_ast_object};
mod asyncio;
pub mod atexit;
mod big_cext;
mod binascii;
mod cext;
pub(crate) mod codecs;
pub(crate) mod collections;
mod colorize;
pub(crate) mod contextvars;
mod csv_;
mod curses_;
mod errno;
mod faulthandler;
mod fcntl_;
mod gc;
mod grp;
mod hashes;
pub(crate) mod imp;
pub(crate) mod io;
pub(crate) mod itertools;
mod lsprof;
pub(crate) mod math;
mod mmap;
mod multibyte_codecs;
mod multiprocessing;
mod opcode_;
mod operator_;
pub(crate) mod os;
mod packaging_compat;
pub(crate) mod pickle;
mod pon_mod;
mod posix;
mod posixshmem;
mod posixsubprocess;
mod pwd;
mod random_;
mod readline;
mod resource;
#[cfg(target_os = "macos")]
mod scproxy;
mod select;
mod sha2;
pub(crate) mod signal;
mod socket_;
mod sre;
mod statistics;
pub(crate) mod stdlib_small;
mod string_mod;
mod struct_;
mod suggestions;
mod symtable;
pub(crate) mod sys;
mod sysconfigdata;
mod syslog;
mod termios;
mod testinternalcapi;
mod testsinglephase;
pub(crate) mod thread;
mod time;
mod tokenize;
mod tracemalloc;
mod unicodedata;
pub(crate) mod weakref;
mod zlib;
mod zoneinfo;

/// Sorted, insert-only lookup table of curated native modules: Python module
/// name -> factory that allocates the module object and installs it into the
/// import cache. Table order is irrelevant to behavior (names are unique);
/// factories must be self-contained and never rely on another row having run.
pub(crate) static NATIVE_MODULES: &[(&str, fn() -> Result<*mut PyObject, String>)] = &[
	("_ast", ast_::make_module),
	("_asyncio", asyncio::make_module),
	("_blake2", hashes::make_blake2_module),
	("_bz2", cext::make_bz2_module),
	("_codecs", codecs::make_module),
	("_codecs_cn", multibyte_codecs::make_codecs_cn_module),
	("_codecs_hk", multibyte_codecs::make_codecs_hk_module),
	("_codecs_iso2022", multibyte_codecs::make_codecs_iso2022_module),
	("_codecs_jp", multibyte_codecs::make_codecs_jp_module),
	("_codecs_kr", multibyte_codecs::make_codecs_kr_module),
	("_codecs_tw", multibyte_codecs::make_codecs_tw_module),
	("_collections", collections::make_module),
	("_colorize", colorize::make_module),
	("_contextvars", contextvars::make_module),
	("_csv", csv_::make_module),
	("_curses", curses_::make_curses_module),
	("_curses_panel", curses_::make_curses_panel_module),
	("_ctypes", big_cext::make_ctypes_underscore_module),
	("_dbm", big_cext::make_dbm_underscore_module),
	("_datetime", stdlib_small::make_datetime_module),
	("_functools", stdlib_small::make_functools_module),
	("_hashlib", cext::make_hashlib_module),
	("_hmac", cext::make_hmac_module),
	("_imp", imp::make_module),
	("_io", io::make_module),
	("_json", stdlib_small::make_json_module),
	("_locale", stdlib_small::make_locale_module),
	("_lsprof", lsprof::make_module),
	("_lzma", cext::make_lzma_module),
	("_md5", hashes::make_md5_module),
	("_multiprocessing", multiprocessing::make_module),
	("_opcode", opcode_::make_module),
	("_operator", operator_::make_module),
	("_pickle", pickle::make_module),
	("_posixshmem", posixshmem::make_module),
	("_posixsubprocess", posixsubprocess::make_module),
	("_random", random_::make_module),
	#[cfg(target_os = "macos")]
	("_scproxy", scproxy::make_module),
	("_sha1", hashes::make_sha1_module),
	("_sha2", sha2::make_module),
	("_sha3", hashes::make_sha3_module),
	("_signal", signal::make_module),
	("_socket", socket_::make_module),
	("_sqlite3", big_cext::make_sqlite3_underscore_module),
	("_ssl", big_cext::make_ssl_underscore_module),
	("_sre", sre::make_module),
	("_statistics", statistics::make_module),
	("_string", string_mod::make_module),
	("_struct", struct_::make_module),
	("_suggestions", suggestions::make_module),
	("_symtable", symtable::make_module),
	("_sysconfig", sysconfigdata::make_sysconfig_module),
	// Name is cfg-selected per target (`_sysconfigdata__darwin_` /
	// `_sysconfigdata__linux_`); the row sorts identically either way.
	(sysconfigdata::MODULE_NAME, sysconfigdata::make_module),
	("_testinternalcapi", testinternalcapi::make_module),
	("_testsinglephase", testsinglephase::make_module),
	("_thread", thread::make_module),
	("_tokenize", tokenize::make_module),
	("_tracemalloc", tracemalloc::make_module),
	("_uuid", cext::make_uuid_module),
	("_warnings", imp::make_warnings_module),
	("_weakref", weakref::make_underscore_module),
	("_zoneinfo", zoneinfo::make_module),
	("_zstd", cext::make_zstd_module),
	("array", array::make_module),
	("atexit", atexit::make_module),
	("binascii", binascii::make_module),
	("builtins", builtins_mod::make_module),
	("errno", errno::make_module),
	("faulthandler", faulthandler::make_module),
	("fcntl", fcntl_::make_module),
	("gc", gc::make_module),
	("grp", grp::make_module),
	("itertools", itertools::make_module),
	("marshal", imp::make_marshal_module),
	("math", math::make_module),
	("mmap", mmap::make_module),
	("os", os::make_module),
	("os.path", os::make_path_module),
	("packaging._manylinux", packaging_compat::make_manylinux_module),
	("packaging._musllinux", packaging_compat::make_musllinux_module),
	("pon", pon_mod::make_module),
	("pwd", pwd::make_module),
	("posix", posix::make_module),
	("readline", readline::make_module),
	("resource", resource::make_module),
	("select", select::make_module),
	("sys", sys::make_module),
	("syslog", syslog::make_module),
	("time", time::make_module),
	("termios", termios::make_module),
	("unicodedata", unicodedata::make_module),
	("zlib", zlib::make_module),
];

/// Modules registered eagerly by [`register_modules`] at runtime init, in
/// registration order. Frozen to the WS-IMPORT six: new native modules are
/// imported lazily; growing this set requires a J0.4 design-doc amendment.
const EAGER_MODULES: &[&str] = &["builtins", "sys", "_io", "time", "os", "_thread"];

/// Creates the named curated module, falling back to installed-package
/// fixtures. `Ok(None)` means "not native": the importer then consults source
/// roots (site-packages, then the vendored stdlib — HANDOFF J0.4 order).
pub(crate) fn make_module(name: &str) -> Result<Option<*mut PyObject>, String> {
	for &(module_name, factory) in NATIVE_MODULES {
		if module_name == name {
			return factory().map(Some);
		}
	}
	installed::make_module(name)
}

/// True when `name` has a curated native factory row in [`NATIVE_MODULES`].
/// Environment-dependent installed-package fixtures are deliberately excluded.
pub(crate) fn is_native_module(name: &str) -> bool {
	NATIVE_MODULES
		.iter()
		.any(|&(module_name, _)| module_name == name)
}

/// Installs the eager curated modules into the import cache once core runtime
/// allocation is available.
pub(crate) fn register_modules() -> Result<(), String> {
	for &name in EAGER_MODULES {
		let name_id = crate::intern::intern(name);
		if crate::import::cached_module(name_id).is_none() {
			let _ = make_module(name)?;
		}
	}
	Ok(())
}
