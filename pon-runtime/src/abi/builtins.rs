//! Builtin helper family namespace.

use crate::{intern::resolve, object::PyObject};

/// Interned builtin selector used by future compact dispatch helpers.
pub type BuiltinId = u16;

/// Loads a builtin by interned name without consulting user local state.
///
/// The lowering emits `LoadBuiltin` for names it statically knows as
/// builtins, but CPython's LOAD_GLOBAL rule is dynamic: the executing
/// function's module globals first, then builtins.  A module may shadow the
/// name at module scope (reprlib's `repr = aRepr.repr`), and that shadow
/// lives ONLY in the defining module's attrs — so this mirrors
/// `pon_load_global` exactly, with the flat map serving genuine builtins
/// last.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pon_load_builtin(name_interned: u32) -> *mut PyObject {
	super::catch_object_helper(|| {
		if let Err(message) = super::ensure_runtime_initialized() {
			return super::return_null_with_error(message);
		}
		super::resolve_global_binding(name_interned)
			.unwrap_or_else(|| {
				let name =
					resolve(name_interned).unwrap_or_else(|| format!("<interned:{name_interned}>"));
				super::exc::raise_name_error_text(&format!("name '{name}' is not defined"))
			})
	})
}
