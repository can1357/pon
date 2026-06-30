//! Deterministic process-local name interning for helper ABI names.
//!
//! Codegen interns names while compiling and passes compact `u32` identifiers to
//! runtime helpers such as `pon_load_global` and `pon_store_global`.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

#[derive(Default)]
struct Interner {
    by_name: HashMap<String, u32>,
    by_id: Vec<String>,
}

static INTERNER: LazyLock<Mutex<Interner>> = LazyLock::new(|| Mutex::new(Interner::default()));

/// Interns `name`, returning a deterministic id within this process.
///
/// The first distinct name receives id `0`, the next receives id `1`, and so on.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interned_names_round_trip() {
        let id = intern("phase_a_name");
        assert_eq!(intern("phase_a_name"), id);
        assert_eq!(resolve(id).as_deref(), Some("phase_a_name"));
    }
}
