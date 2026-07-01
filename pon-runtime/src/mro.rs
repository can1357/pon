//! C3 method-resolution-order support for heap type objects.
//!
//! The module is intentionally self-contained: type creation owns the final
//! `tp_mro` pointer, while attribute lookup can borrow the frozen entries without
//! knowing how the linearization was produced.

use core::ptr;

use crate::object::{PyObject, PyObjectHeader, PyType};
use crate::thread_state::pon_err_set;

/// Boxed runtime carrier stored in `PyType::tp_mro`.
#[repr(C)]
#[derive(Debug)]
pub struct PyMro {
    /// Common object header.  `tp_mro` is an internal carrier, so the type may be
    /// NULL until the integration hub gives it a concrete Python-visible type.
    pub ob_base: PyObjectHeader,
    entries: Vec<*mut PyType>,
}

impl PyMro {
    /// Borrow the C3 linearization entries.
    #[must_use]
    pub fn entries(&self) -> &[*mut PyType] {
        &self.entries
    }
}

/// Returns true when `candidate` is `base` or inherits from `base` through C3 MRO.
#[must_use]
pub unsafe fn is_subtype(candidate: *mut PyType, base: *mut PyType) -> bool {
    if candidate.is_null() || base.is_null() {
        return false;
    }
    if candidate == base {
        return true;
    }
    for ty in unsafe { mro_entries(candidate) } {
        if ty == base {
            return true;
        }
    }
    false
}

/// Store a freshly computed MRO on `ty`.
///
/// Returns `0` on success and `-1` with the current thread-state error set when
/// C3 cannot produce a consistent order.
pub unsafe fn set_c3_mro(ty: *mut PyType, bases: &[*mut PyType]) -> i32 {
    if ty.is_null() {
        pon_err_set("cannot compute MRO for NULL type");
        return -1;
    }

    let Some(entries) = (unsafe { compute_c3_mro(ty, bases) }) else {
        return -1;
    };
    let carrier = Box::into_raw(Box::new(PyMro {
        ob_base: PyObjectHeader::new(ptr::null()),
        entries,
    }));
    unsafe {
        (*ty).tp_mro = carrier.cast::<PyObject>();
    }
    0
}

/// Borrow MRO entries from a type.  Static/bootstrap types without `tp_mro`
/// fall back to their `tp_base` chain.
#[must_use]
pub unsafe fn mro_entries(ty: *mut PyType) -> Vec<*mut PyType> {
    if ty.is_null() {
        return Vec::new();
    }
    let carrier = unsafe { (*ty).tp_mro };
    if !carrier.is_null() {
        let mro = unsafe { &*carrier.cast::<PyMro>() };
        return mro.entries.clone();
    }

    let mut out = Vec::new();
    let mut current = ty;
    while !current.is_null() {
        out.push(current);
        current = unsafe { (*current).tp_base };
    }
    out
}

/// Compute CPython-style C3 linearization for `ty + bases`.
#[must_use]
pub unsafe fn compute_c3_mro(ty: *mut PyType, bases: &[*mut PyType]) -> Option<Vec<*mut PyType>> {
    if ty.is_null() {
        pon_err_set("cannot compute MRO for NULL type");
        return None;
    }
    for base in bases {
        if base.is_null() {
            pon_err_set("class base is NULL");
            return None;
        }
    }

    let mut sequences: Vec<Vec<*mut PyType>> = bases.iter().map(|base| unsafe { mro_entries(*base) }).collect();
    sequences.push(bases.to_vec());

    let mut result = vec![ty];
    while sequences.iter().any(|seq| !seq.is_empty()) {
        let mut candidate = ptr::null_mut();
        'candidates: for seq in &sequences {
            if seq.is_empty() {
                continue;
            }
            let head = seq[0];
            for other in &sequences {
                if other.len() > 1 && other[1..].contains(&head) {
                    continue 'candidates;
                }
            }
            candidate = head;
            break;
        }

        if candidate.is_null() {
            pon_err_set("Cannot create a consistent method resolution order");
            return None;
        }

        result.push(candidate);
        for seq in &mut sequences {
            if seq.first().copied() == Some(candidate) {
                seq.remove(0);
            }
        }
    }

    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::PyType;

    #[test]
    fn c3_linearizes_diamond() {
        let mut type_type = PyType::new(ptr::null(), "type", core::mem::size_of::<PyType>());
        let type_ptr = &mut type_type as *mut PyType;
        type_type.ob_base.ob_type = type_ptr;

        let mut object = PyType::new(type_ptr, "object", 0);
        let object_ptr = &mut object as *mut PyType;
        let mut a = PyType::new(type_ptr, "A", 0);
        a.tp_base = object_ptr;
        let a_ptr = &mut a as *mut PyType;
        let mut b = PyType::new(type_ptr, "B", 0);
        b.tp_base = a_ptr;
        let b_ptr = &mut b as *mut PyType;
        let mut c = PyType::new(type_ptr, "C", 0);
        c.tp_base = a_ptr;
        let c_ptr = &mut c as *mut PyType;
        let mut d = PyType::new(type_ptr, "D", 0);
        let d_ptr = &mut d as *mut PyType;

        unsafe {
            assert_eq!(set_c3_mro(a_ptr, &[object_ptr]), 0);
            assert_eq!(set_c3_mro(b_ptr, &[a_ptr]), 0);
            assert_eq!(set_c3_mro(c_ptr, &[a_ptr]), 0);
            assert_eq!(set_c3_mro(d_ptr, &[b_ptr, c_ptr]), 0);
            let names: Vec<_> = mro_entries(d_ptr).iter().map(|ty| unsafe { (**ty).name() }).collect();
            assert_eq!(names, ["D", "B", "C", "A", "object"]);
        }
    }
}
