pub mod builtins_mod;
mod installed;

use crate::object::PyObject;

pub(crate) use crate::import::install_module;

mod io;
mod os;
mod sys;
mod time;
mod thread;

pub(crate) fn make_module(name: &str) -> Result<Option<*mut PyObject>, String> {
    match name {
        "builtins" => builtins_mod::make_module().map(Some),
        "sys" => sys::make_module().map(Some),
        "_io" => io::make_module().map(Some),
        "time" => time::make_module().map(Some),
        "os" => os::make_module().map(Some),
        "_thread" => thread::make_module().map(Some),
        _ => installed::make_module(name),
    }
}

pub(crate) fn register_modules() -> Result<(), String> {
    for name in ["builtins", "sys", "_io", "time", "os", "_thread"] {
        let name_id = crate::intern::intern(name);
        if crate::import::cached_module(name_id).is_none() {
            let _ = make_module(name)?;
        }
    }
    Ok(())
}
