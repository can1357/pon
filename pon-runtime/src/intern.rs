//! Deterministic process-local name interning for helper ABI names.
//!
//! Codegen interns names while compiling and passes compact `u32` identifiers
//! to runtime helpers such as `pon_load_global` and `pon_store_global`.

use std::{
	collections::HashMap,
	sync::{LazyLock, Mutex},
};

#[derive(Default)]
struct Interner {
	by_name: HashMap<String, u32>,
	by_id:   Vec<String>,
}

static INTERNER: LazyLock<Mutex<Interner>> = LazyLock::new(|| Mutex::new(Interner::default()));

/// Interns `name`, returning a deterministic id within this process.
///
/// The first distinct name receives id `0`, the next receives id `1`, and so
/// on.
#[must_use]
pub fn intern(name: &str) -> u32 {
	let mut interner = INTERNER.lock().unwrap_or_else(|poison| poison.into_inner());
	if let Some(id) = interner.by_name.get(name).copied() {
		return id;
	}

	let id = interner.by_id.len() as u32;
	let owned = name.to_owned();
	interner.by_id.push(owned.clone());
	interner.by_name.insert(owned, id);
	id
}

/// Resolves an interned id back to its name.
#[must_use]
pub fn resolve(id: u32) -> Option<String> {
	let interner = INTERNER.lock().unwrap_or_else(|poison| poison.into_inner());
	interner.by_id.get(id as usize).cloned()
}

/// Snapshot of every interned name in id order (index `i` holds id `i`).
///
/// AoT builds replay this snapshot in the produced executable so name ids baked
/// into object data resolve to the same strings in the fresh process interner.
#[must_use]
pub fn snapshot() -> Vec<String> {
	let interner = INTERNER.lock().unwrap_or_else(|poison| poison.into_inner());
	interner.by_id.clone()
}

/// Internable spelling for Python's addition dunder.
pub const DUNDER_ADD: &str = "__add__";
/// Internable spelling for Python's reflected addition dunder.
pub const DUNDER_RADD: &str = "__radd__";
/// Internable spelling for Python's iterator-construction dunder.
pub const DUNDER_ITER: &str = "__iter__";
/// Internable spelling for Python's iterator-next dunder.
pub const DUNDER_NEXT: &str = "__next__";
/// Internable spelling for Python's length dunder.
pub const DUNDER_LEN: &str = "__len__";
/// Internable spelling for Python's item lookup dunder.
pub const DUNDER_GETITEM: &str = "__getitem__";
/// Internable spelling for Python's item assignment/deletion dunder.
pub const DUNDER_SETITEM: &str = "__setitem__";
/// Internable spelling for Python's call dunder.
pub const DUNDER_CALL: &str = "__call__";
/// Internable spelling for Python's descriptor get dunder.
pub const DUNDER_GET: &str = "__get__";
/// Internable spelling for Python's descriptor set dunder.
pub const DUNDER_SET: &str = "__set__";

/// Interns and returns the deterministic id for [`DUNDER_ADD`].
#[must_use]
pub fn dunder_add() -> u32 {
	intern(DUNDER_ADD)
}

/// Interns and returns the deterministic id for [`DUNDER_RADD`].
#[must_use]
pub fn dunder_radd() -> u32 {
	intern(DUNDER_RADD)
}

/// Interns and returns the deterministic id for [`DUNDER_ITER`].
#[must_use]
pub fn dunder_iter() -> u32 {
	intern(DUNDER_ITER)
}

/// Interns and returns the deterministic id for [`DUNDER_NEXT`].
#[must_use]
pub fn dunder_next() -> u32 {
	intern(DUNDER_NEXT)
}

/// Interns and returns the deterministic id for [`DUNDER_LEN`].
#[must_use]
pub fn dunder_len() -> u32 {
	intern(DUNDER_LEN)
}

/// Interns and returns the deterministic id for [`DUNDER_GETITEM`].
#[must_use]
pub fn dunder_getitem() -> u32 {
	intern(DUNDER_GETITEM)
}

/// Interns and returns the deterministic id for [`DUNDER_SETITEM`].
#[must_use]
pub fn dunder_setitem() -> u32 {
	intern(DUNDER_SETITEM)
}

/// Interns and returns the deterministic id for [`DUNDER_CALL`].
#[must_use]
pub fn dunder_call() -> u32 {
	intern(DUNDER_CALL)
}

/// Interns and returns the deterministic id for [`DUNDER_GET`].
#[must_use]
pub fn dunder_get() -> u32 {
	intern(DUNDER_GET)
}

/// Interns and returns the deterministic id for [`DUNDER_SET`].
#[must_use]
pub fn dunder_set() -> u32 {
	intern(DUNDER_SET)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn interned_names_round_trip() {
		let id = intern("phase_a_name");
		assert_eq!(intern("phase_a_name"), id);
		assert_eq!(resolve(id).as_deref(), Some("phase_a_name"));
	}

	#[test]
	fn dunder_helpers_use_the_deterministic_interner() {
		assert_eq!(dunder_add(), intern(DUNDER_ADD));
		assert_eq!(dunder_radd(), intern(DUNDER_RADD));
		assert_eq!(dunder_iter(), intern(DUNDER_ITER));
		assert_eq!(dunder_next(), intern(DUNDER_NEXT));
		assert_eq!(dunder_len(), intern(DUNDER_LEN));
		assert_eq!(dunder_getitem(), intern(DUNDER_GETITEM));
		assert_eq!(dunder_setitem(), intern(DUNDER_SETITEM));
		assert_eq!(dunder_call(), intern(DUNDER_CALL));
		assert_eq!(dunder_get(), intern(DUNDER_GET));
		assert_eq!(dunder_set(), intern(DUNDER_SET));
		assert_eq!(resolve(dunder_add()).as_deref(), Some(DUNDER_ADD));
	}
}
