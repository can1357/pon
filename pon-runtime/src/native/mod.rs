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

pub mod builtins_mod;
pub mod builtins_batch;
mod installed;

use crate::object::PyObject;

pub(crate) use crate::import::install_module;

mod array;
mod ast_;
pub mod atexit;
mod binascii;
pub(crate) mod codecs;
pub(crate) mod collections;
mod colorize;
pub(crate) mod contextvars;
mod errno;
mod gc;
mod imp;
mod io;
mod itertools;
mod math;
mod opcode_;
mod os;
mod posix;
mod random_;
mod select;
mod sha2;
pub(crate) mod signal;
mod sre;
mod string_mod;
mod struct_;
mod sys;
mod sysconfigdata;
pub(crate) mod thread;
mod time;
mod tokenize;
pub(crate) mod weakref;

/// Sorted, insert-only lookup table of curated native modules: Python module
/// name -> factory that allocates the module object and installs it into the
/// import cache. Table order is irrelevant to behavior (names are unique);
/// factories must be self-contained and never rely on another row having run.
pub(crate) static NATIVE_MODULES: &[(&str, fn() -> Result<*mut PyObject, String>)] = &[
    ("_ast", ast_::make_module),
    ("_codecs", codecs::make_module),
    ("_collections", collections::make_module),
    ("_colorize", colorize::make_module),
    ("_contextvars", contextvars::make_module),
    ("_imp", imp::make_module),
    ("_io", io::make_module),
    ("_opcode", opcode_::make_module),
    ("_random", random_::make_module),
    ("_sha2", sha2::make_module),
    ("_signal", signal::make_module),
    ("_sre", sre::make_module),
    ("_string", string_mod::make_module),
    ("_struct", struct_::make_module),
    // Name is cfg-selected per target (`_sysconfigdata__darwin_` /
    // `_sysconfigdata__linux_`); the row sorts identically either way.
    (sysconfigdata::MODULE_NAME, sysconfigdata::make_module),
    ("_thread", thread::make_module),
    ("_tokenize", tokenize::make_module),
    ("_warnings", imp::make_warnings_module),
    ("_weakref", weakref::make_underscore_module),
    ("array", array::make_module),
    ("atexit", atexit::make_module),
    ("binascii", binascii::make_module),
    ("builtins", builtins_mod::make_module),
    ("errno", errno::make_module),
    ("gc", gc::make_module),
    ("itertools", itertools::make_module),
    ("marshal", imp::make_marshal_module),
    ("math", math::make_module),
    ("os", os::make_module),
    ("os.path", os::make_path_module),
    ("posix", posix::make_module),
    ("select", select::make_module),
    ("sys", sys::make_module),
    ("time", time::make_module),
    ("weakref", weakref::make_module),
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
    NATIVE_MODULES.iter().any(|&(module_name, _)| module_name == name)
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
