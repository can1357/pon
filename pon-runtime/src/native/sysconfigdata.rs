//! Native `_sysconfigdata__darwin_` / `_sysconfigdata__linux_` module: pon's
//! build-time configuration (`sysconfig` ladder, CT wave 4).
//!
//! CPython does not ship this module in `Lib/`: the build GENERATES
//! `_sysconfigdata_{abiflags}_{platform}_{multiarch}.py` from the configured
//! Makefile and installs it beside the stdlib, so the vendored tree cannot
//! serve it by design.  pon's build configuration is fixed when rustc
//! compiles the runtime, so a curated registry row is the exact analogue of
//! CPython's generation step.
//!
//! Why a native module and not a `sysconfig` fallback: 3.14's
//! `sysconfig._init_posix` is `vars.update(_get_sysconfigdata() | vars)` with
//! no except arm anywhere up through `get_config_vars()` — a missing module
//! is an ImportError out of every public sysconfig API, and `test.support`
//! calls `get_config_var` at module scope (`Py_GIL_DISABLED`,
//! `TEST_MODULES_ENABLED`, `MISSING_C_DOCSTRINGS`), so the module gates the
//! whole `test.support` import ladder.
//!
//! Name: `sysconfig._get_sysconfigdata_name()` computes
//! `f'_sysconfigdata_{sys.abiflags}_{sys.platform}_{multiarch}'`; under pon
//! `sys.abiflags == ''` and `sys.implementation` exposes no `_multiarch`, so
//! the name is `_sysconfigdata__darwin_` (macOS) or `_sysconfigdata__linux_`
//! (Linux CI).  Other targets keep an inert name nothing ever requests:
//! Windows takes `_init_non_posix` and never imports sysconfigdata at all.
//!
//! `build_time_vars` is the minimal audited key set — every row names its
//! consumer.  `sysconfig.get_config_var` of an absent key returns `None`
//! (plain `dict.get`), which is CPython's own behavior for a var absent from
//! a particular build's Makefile, so omitted keys are behavior-correct, not
//! gaps.  Documented divergence: the module is a pon builtin (listed in
//! `sys.builtin_module_names`, no `__file__`), where CPython serves a
//! generated `.py` source module; values are pon's, not any CPython build's,
//! so they are asserted by unit tests here, never by differential corpus.

use crate::intern::intern;
use crate::object::PyObject;

use super::install_module;

/// Import name served by this row (see module docs for the derivation).
pub(super) const MODULE_NAME: &str = if cfg!(target_os = "macos") {
    "_sysconfigdata__darwin_"
} else if cfg!(target_os = "linux") {
    "_sysconfigdata__linux_"
} else {
    // Inert: `sysconfig` computes a platform-specific name that never
    // matches this one, so the row is unreachable on unsupported targets.
    "_sysconfigdata__unsupported_"
};

const PON_INCLUDEPY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/include");
const PON_EXT_SUFFIX: &str = ".pon.so";
#[cfg(test)]
const PON_CAPI_BOOTSTRAP: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/capi/pon_capi_bootstrap.c");

/// One `build_time_vars` value.  CPython's generated dict holds strings and
/// ints: flag vars parse as ints, while `sysconfig._ALWAYS_STR` names
/// (`MACOSX_DEPLOYMENT_TARGET`) must stay strings.
enum VarValue {
    Str(&'static str),
    Int(i64),
}

/// Audited `build_time_vars` entries, strictly sorted by key — CPython's
/// generator pprints the dict sorted and iteration order is observable.
/// Grow this table only with a named consumer, keeping the sort.
fn build_time_vars() -> Vec<(&'static str, VarValue)> {
    use VarValue::{Int, Str};

    let macos = cfg!(target_os = "macos");
    let mut vars = vec![
        // `sysconfig._init_config_vars` cross-build arm hard-derefs
        // `_CONFIG_VARS['ABIFLAGS']` when `_PYTHON_PROJECT_BASE` is set;
        // mirrors `sys.abiflags` ('').
        ("ABIFLAGS", Str("")),
        // Distutils-style build backends use this to link extension modules.
        // The bootstrap source is linked into each extension so the loader can
        // inject Pon's C-API table without relying on executable symbol export.
        (
            "BLDSHARED",
            Str(if macos {
                concat!(
                    "clang -bundle -undefined dynamic_lookup -I",
                    env!("CARGO_MANIFEST_DIR"),
                    "/include ",
                    env!("CARGO_MANIFEST_DIR"),
                    "/capi/pon_capi_bootstrap.c"
                )
            } else {
                concat!(
                    "cc -shared -I",
                    env!("CARGO_MANIFEST_DIR"),
                    "/include ",
                    env!("CARGO_MANIFEST_DIR"),
                    "/capi/pon_capi_bootstrap.c"
                )
            }),
        ),
        // `test.support.check_sanitizer` reads it, and
        // `_osx_support` scans it for `-isysroot`/`-arch`.  Pon compiles
        // Python through Cranelift, not a C compiler.
        ("CC", Str(if macos { "clang" } else { "cc" })),
        // Extra compile flags for shared extension objects.
        ("CCSHARED", Str(concat!("-I", env!("CARGO_MANIFEST_DIR"), "/include"))),
        // Header search path for recompiled extensions including `Python.h`.
        ("CFLAGS", Str(concat!("-I", env!("CARGO_MANIFEST_DIR"), "/include"))),
        // `test.support.check_sanitizer` / `check_bolt_optimized`: pon was
        // not configured with sanitizers or BOLT.
        ("CONFIG_ARGS", Str("")),
        // Header roots used by sysconfig/distutils.
        ("CONFINCLUDEPY", Str(PON_INCLUDEPY)),
        // Extensions compiled for Pon's source C-API shim use a dedicated
        // suffix so they never masquerade as CPython ABI wheels.
        ("EXT_SUFFIX", Str(PON_EXT_SUFFIX)),
        ("INCLUDEPY", Str(PON_INCLUDEPY)),
        (
            "LDSHARED",
            Str(if macos {
                concat!(
                    "clang -bundle -undefined dynamic_lookup -I",
                    env!("CARGO_MANIFEST_DIR"),
                    "/include ",
                    env!("CARGO_MANIFEST_DIR"),
                    "/capi/pon_capi_bootstrap.c"
                )
            } else {
                concat!(
                    "cc -shared -I",
                    env!("CARGO_MANIFEST_DIR"),
                    "/include ",
                    env!("CARGO_MANIFEST_DIR"),
                    "/capi/pon_capi_bootstrap.c"
                )
            }),
        ),
    ];
    if macos {
        // `_osx_support.get_platform_osx` (via `sysconfig.get_platform`):
        // rustc's default deployment target for the compiled runtime
        // (aarch64-apple-darwin 11.0, x86_64-apple-darwin 10.12).  In
        // `sysconfig._ALWAYS_STR`: never int-converted.
        vars.push((
            "MACOSX_DEPLOYMENT_TARGET",
            Str(if cfg!(target_arch = "aarch64") { "11.0" } else { "10.12" }),
        ));
    }
    vars.extend([
        // `test.support.check_cflags_pgo`: pon has no PGO instrumentation.
        ("PGO_PROF_USE_FLAG", Str("")),
        // `test.support.python_is_optimized`: optimizing Python code is the
        // JIT's business, not a C-compiler flag; '' reports the conservative
        // "not a C-optimized build".
        ("PY_CFLAGS", Str(concat!("-I", env!("CARGO_MANIFEST_DIR"), "/include"))),
        // `test.support.check_cflags_pgo`.
        ("PY_CFLAGS_NODIST", Str("")),
        // Module-scope `test.support.Py_GIL_DISABLED` and sysconfig's
        // `abi_thread` ('' for a GIL build): pon is not free-threaded.
        ("Py_GIL_DISABLED", Int(0)),
        // Generic shared-library suffix and ABI tag used by sysconfig callers.
        ("SHLIB_SUFFIX", Str(".so")),
        ("SOABI", Str("pon-314")),
        // Module-scope `test.support.TEST_MODULES_ENABLED`: pon ships none
        // of CPython's C test modules (`_testcapi`, `_testinternalcapi`, …)
        // — exactly what a `--disable-test-modules` CPython reports — so
        // `@requires_test_modules` units skip cleanly instead of failing an
        // import.
        ("TEST_MODULES", Str("no")),
        // Module-scope `test.support.MISSING_C_DOCSTRINGS` arm: pon serves
        // docstrings on its native surface.
        ("WITH_DOC_STRINGS", Int(1)),
        // `sysconfig._installation_is_relocated` hard-derefs both; equal to
        // pon's `sys.base_prefix`/`sys.base_exec_prefix` ('') so the
        // installation reports "not relocated".
        ("exec_prefix", Str("")),
        ("prefix", Str("")),
    ]);
    vars
}

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let vars = build_time_vars();
    let mut pairs: Vec<*mut PyObject> = Vec::with_capacity(vars.len() * 2);
    for (key, value) in &vars {
        // SAFETY: allocation helpers return NULL with a diagnostic on failure.
        let key_object = unsafe { crate::abi::pon_const_str(key.as_ptr(), key.len()) };
        if key_object.is_null() {
            return Err(format!("failed to allocate build_time_vars key '{key}'"));
        }
        let value_object = match value {
            // SAFETY: as above; NULL is checked below.
            VarValue::Str(text) => unsafe { crate::abi::pon_const_str(text.as_ptr(), text.len()) },
            // SAFETY: as above; NULL is checked below.
            VarValue::Int(number) => unsafe { crate::abi::pon_const_int(*number) },
        };
        if value_object.is_null() {
            return Err(format!("failed to allocate build_time_vars value for '{key}'"));
        }
        pairs.push(key_object);
        pairs.push(value_object);
    }
    // SAFETY: `pairs` holds `vars.len()` live key/value pairs.
    let build_time_vars = unsafe { crate::abi::map::pon_build_map(pairs.as_mut_ptr(), vars.len()) };
    if build_time_vars.is_null() {
        return Err("failed to allocate build_time_vars dict".to_owned());
    }
    install_module(MODULE_NAME, [(intern("build_time_vars"), build_time_vars)])
}

#[cfg(test)]
mod tests {
    use std::ptr;

    use super::{MODULE_NAME, PON_CAPI_BOOTSTRAP, PON_EXT_SUFFIX, PON_INCLUDEPY, VarValue, build_time_vars};
    use crate::abi::map::pon_dict_get_item;
    use crate::abi::{format_object_for_print, pon_const_str, pon_runtime_init};
    use crate::import::{pon_import_from, pon_import_name, reset_import_state_for_tests};
    use crate::intern::intern;
    use crate::thread_state::{pon_err_clear, pon_err_message, test_state_lock};

    struct ResetImportStateOnDrop;

    impl Drop for ResetImportStateOnDrop {
        fn drop(&mut self) {
            reset_import_state_for_tests();
        }
    }

    #[test]
    fn module_name_matches_sysconfig_derivation() {
        // `f'_sysconfigdata_{sys.abiflags}_{sys.platform}_{multiarch}'` with
        // pon's abiflags == '' and no `sys.implementation._multiarch`.
        if cfg!(target_os = "macos") {
            assert_eq!(MODULE_NAME, "_sysconfigdata__darwin_");
        } else if cfg!(target_os = "linux") {
            assert_eq!(MODULE_NAME, "_sysconfigdata__linux_");
        }
        assert!(MODULE_NAME.starts_with("_sysconfigdata__"));
        assert!(MODULE_NAME.ends_with('_'));
    }

    #[test]
    fn build_time_vars_table_is_sorted_and_audited() {
        let vars = build_time_vars();
        assert!(
            vars.windows(2).all(|pair| pair[0].0 < pair[1].0),
            "build_time_vars keys must be strictly sorted and unique"
        );
        let entry = |name: &str| vars.iter().find(|(key, _)| *key == name).map(|(_, value)| value);
        for required in [
            "BLDSHARED",
            "CFLAGS",
            "CONFINCLUDEPY",
            "CONFIG_ARGS",
            "EXT_SUFFIX",
            "INCLUDEPY",
            "LDSHARED",
            "Py_GIL_DISABLED",
            "SHLIB_SUFFIX",
            "SOABI",
            "TEST_MODULES",
            "WITH_DOC_STRINGS",
            "exec_prefix",
            "prefix",
        ] {
            assert!(entry(required).is_some(), "missing audited key {required}");
        }
        let expected_cflags = format!("-I{PON_INCLUDEPY}");
        assert!(matches!(entry("CFLAGS"), Some(VarValue::Str(value)) if *value == expected_cflags.as_str()));
        assert!(matches!(entry("CONFINCLUDEPY"), Some(VarValue::Str(value)) if *value == PON_INCLUDEPY));
        assert!(matches!(entry("EXT_SUFFIX"), Some(VarValue::Str(value)) if *value == PON_EXT_SUFFIX));
        assert!(matches!(entry("INCLUDEPY"), Some(VarValue::Str(value)) if *value == PON_INCLUDEPY));
        assert!(matches!(entry("SHLIB_SUFFIX"), Some(VarValue::Str(".so"))));
        assert!(matches!(entry("SOABI"), Some(VarValue::Str("pon-314"))));
        assert!(matches!(entry("prefix"), Some(VarValue::Str(""))));
        assert!(matches!(entry("exec_prefix"), Some(VarValue::Str(""))));
        assert!(matches!(entry("Py_GIL_DISABLED"), Some(VarValue::Int(0))));
        // The darwin-only deployment target follows the compile target.
        assert_eq!(entry("MACOSX_DEPLOYMENT_TARGET").is_some(), cfg!(target_os = "macos"));
    }

    #[test]
    fn import_serves_build_time_vars_dict() {
        let _guard = test_state_lock();
        let _reset = ResetImportStateOnDrop;
        unsafe {
            assert_eq!(pon_runtime_init(), 0);
        }
        pon_err_clear();
        reset_import_state_for_tests();

        let module = unsafe { pon_import_name(intern(MODULE_NAME), ptr::null(), 0, 0) };
        assert!(!module.is_null(), "importing {MODULE_NAME} failed: {:?}", pon_err_message());

        let vars_dict = unsafe { pon_import_from(module, intern("build_time_vars")) };
        assert!(!vars_dict.is_null(), "build_time_vars attr missing: {:?}", pon_err_message());

        let lookup = |name: &str| {
            // SAFETY: allocation helper; asserted non-NULL below.
            let key = unsafe { pon_const_str(name.as_ptr(), name.len()) };
            assert!(!key.is_null(), "failed to allocate lookup key {name}");
            // SAFETY: `vars_dict` is a live dict and `key` a live string.
            unsafe { pon_dict_get_item(vars_dict, key) }
        };

        let includepy = lookup("INCLUDEPY");
        assert!(!includepy.is_null(), "INCLUDEPY missing from build_time_vars");
        assert_eq!(format_object_for_print(includepy).as_deref(), Ok(PON_INCLUDEPY));

        let ext_suffix = lookup("EXT_SUFFIX");
        assert!(!ext_suffix.is_null(), "EXT_SUFFIX missing from build_time_vars");
        assert_eq!(format_object_for_print(ext_suffix).as_deref(), Ok(PON_EXT_SUFFIX));

        let build_shared = lookup("BLDSHARED");
        assert!(!build_shared.is_null(), "BLDSHARED missing from build_time_vars");
        let build_shared = format_object_for_print(build_shared).expect("BLDSHARED text");
        assert!(build_shared.contains(PON_CAPI_BOOTSTRAP), "BLDSHARED missing bootstrap source: {build_shared}");
        let expected_builder = if cfg!(target_os = "macos") { "clang -bundle" } else { "cc -shared" };
        assert!(build_shared.contains(expected_builder), "BLDSHARED missing expected linker form: {build_shared}");
        assert!(build_shared.contains(PON_INCLUDEPY), "BLDSHARED missing Pon include path: {build_shared}");

        let cflags = lookup("CFLAGS");
        assert!(!cflags.is_null(), "CFLAGS missing from build_time_vars");
        let expected_cflags = format!("-I{PON_INCLUDEPY}");
        assert_eq!(format_object_for_print(cflags).as_deref(), Ok(expected_cflags.as_str()));

        let cc = lookup("CC");
        assert!(!cc.is_null(), "CC missing from build_time_vars");
        let expected_cc = if cfg!(target_os = "macos") { "clang" } else { "cc" };
        assert_eq!(format_object_for_print(cc).as_deref(), Ok(expected_cc));

        let gil = lookup("Py_GIL_DISABLED");
        assert!(!gil.is_null(), "Py_GIL_DISABLED missing from build_time_vars");
        assert_eq!(format_object_for_print(gil).as_deref(), Ok("0"));
    }
}
