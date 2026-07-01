//! Builtin helper family namespace.

use crate::intern::resolve;
use crate::object::PyObject;

/// Interned builtin selector used by future compact dispatch helpers.
pub type BuiltinId = u16;

/// Loads a builtin by interned name without consulting user local state.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_load_builtin(name_interned: u32) -> *mut PyObject {
    super::catch_object_helper(|| {
        if let Err(message) = super::ensure_runtime_initialized() {
            return super::return_null_with_error(message);
        }
        super::with_runtime(|runtime| runtime.globals.get(&name_interned).copied())
            .flatten()
            .unwrap_or_else(|| {
                let name = resolve(name_interned).unwrap_or_else(|| format!("<interned:{name_interned}>"));
                super::return_null_with_error(format!("builtin name '{name}' is not defined"))
            })
    })
}
