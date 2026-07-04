//! `_zoneinfo` compatibility backed by `Lib/zoneinfo/_zoneinfo.py`.
//!
//! The CPython C extension accelerates the same `ZoneInfo` behavior implemented
//! in the vendored pure-Python module. Pon has no native tzfile parser cache, so
//! direct `_zoneinfo` imports expose the stdlib implementation while imports
//! made by `zoneinfo.__init__` still fall through to its normal fallback path.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::intern::{intern, resolve};
use crate::object::PyObject;
use crate::thread_state::{pon_err_clear, pon_err_message};

use super::install_module;

static BUILDING: AtomicBool = AtomicBool::new(false);

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    if active_zoneinfo_import() || BUILDING.swap(true, Ordering::AcqRel) {
        return Err("No module named '_zoneinfo'".to_owned());
    }

    let result = build_module();
    BUILDING.store(false, Ordering::Release);
    result
}

fn build_module() -> Result<*mut PyObject, String> {
    import_exact("zoneinfo._zoneinfo")?;
    let zone_info = crate::import::module_attr(intern("zoneinfo._zoneinfo"), intern("ZoneInfo"))
        .ok_or_else(|| "zoneinfo._zoneinfo.ZoneInfo is unavailable".to_owned())?;
    install_module(
        "_zoneinfo",
        [(intern("__name__"), str_object("_zoneinfo")?), (intern("ZoneInfo"), zone_info)],
    )
}

fn active_zoneinfo_import() -> bool {
    crate::import::active_module_name_id()
        .and_then(resolve)
        .is_some_and(|name| name == "zoneinfo" || name.starts_with("zoneinfo."))
}

fn import_exact(name: &str) -> Result<*mut PyObject, String> {
    let module = crate::import::import_named_module_raw(name);
    if module.is_null() {
        let message = pon_err_message().unwrap_or_else(|| format!("failed to import {name}"));
        pon_err_clear();
        Err(message)
    } else {
        Ok(module)
    }
}

fn str_object(text: &str) -> Result<*mut PyObject, String> {
    let object = unsafe { crate::abi::pon_const_str(text.as_ptr(), text.len()) };
    (!object.is_null()).then_some(object).ok_or_else(|| format!("failed to allocate string {text:?}"))
}
