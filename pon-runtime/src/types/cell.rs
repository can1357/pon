//! Closure cell implementation.
//!
//! Cells are deliberately tiny: they hold one boxed `PyObject` pointer and
//! expose get/set/delete helpers with explicit error returns.  The central
//! object layout integration will decide whether these become first-class GC
//! allocations or remain frame-owned storage; the semantics here already match
//! Python closure by-reference mutation.

use std::ptr;

use crate::object::PyObject;

/// Function-closure cell storing one boxed Python value.
#[repr(C)]
#[derive(Debug)]
pub struct PyCell {
	value: *mut PyObject,
}

impl PyCell {
	/// Create a new closure cell.
	#[must_use]
	pub fn new(value: *mut PyObject) -> Self {
		Self { value }
	}

	/// Return the current value or `None` when the cell is empty.
	#[must_use]
	pub fn get(&self) -> Option<*mut PyObject> {
		(!self.value.is_null()).then_some(self.value)
	}

	/// Store a new boxed value in the cell.
	pub fn set(&mut self, value: *mut PyObject) -> Result<(), String> {
		if value.is_null() {
			return Err("cannot store NULL in a closure cell".to_owned());
		}
		self.value = value;
		Ok(())
	}

	/// Empty the cell, used by `del` on a cell variable.
	pub fn delete(&mut self) {
		self.value = ptr::null_mut();
	}
}

/// Allocate a frame/lowering-owned closure cell.
pub fn new_cell(value: *mut PyObject) -> *mut PyCell {
	Box::into_raw(Box::new(PyCell::new(value)))
}

/// Release a cell allocated with [`new_cell`].
pub unsafe fn drop_cell(cell: *mut PyCell) {
	if !cell.is_null() {
		// SAFETY: The caller promises the pointer came from `Box::into_raw`.
		unsafe {
			drop(Box::from_raw(cell));
		}
	}
}

/// Read a cell value.
pub unsafe fn cell_get(cell: *mut PyCell) -> Result<*mut PyObject, String> {
	if cell.is_null() {
		return Err("closure cell pointer is null".to_owned());
	}
	// SAFETY: The caller supplied a live cell pointer.
	unsafe {
		(*cell)
			.get()
			.ok_or_else(|| "free variable referenced before assignment".to_owned())
	}
}

/// Write a cell value.
pub unsafe fn cell_set(cell: *mut PyCell, value: *mut PyObject) -> Result<(), String> {
	if cell.is_null() {
		return Err("closure cell pointer is null".to_owned());
	}
	// SAFETY: The caller supplied a live cell pointer.
	unsafe { (*cell).set(value) }
}

/// Delete a cell value.
pub unsafe fn cell_delete(cell: *mut PyCell) -> Result<(), String> {
	if cell.is_null() {
		return Err("closure cell pointer is null".to_owned());
	}
	// SAFETY: The caller supplied a live cell pointer.
	unsafe {
		(*cell).delete();
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn cell_mutates_by_reference() {
		let first = 0x10usize as *mut PyObject;
		let second = 0x20usize as *mut PyObject;
		let cell = new_cell(first);
		unsafe {
			assert_eq!(cell_get(cell).unwrap(), first);
			cell_set(cell, second).unwrap();
			assert_eq!(cell_get(cell).unwrap(), second);
			cell_delete(cell).unwrap();
			assert!(cell_get(cell).unwrap_err().contains("before assignment"));
			drop_cell(cell);
		}
	}
}
