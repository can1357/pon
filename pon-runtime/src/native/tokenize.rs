//! Native `_tokenize` seed (WS-IMPORT: `tokenize.py` -> `traceback` ->
//! `unittest`).
//!
//! CPython's `_tokenize` is the C tokenizer (`Python/Python-tokenize.c`);
//! `Lib/tokenize.py` imports it at module scope but only *instantiates*
//! `_tokenize.TokenizerIter` when a caller actually tokenizes source (e.g.
//! `traceback`'s NameError/SyntaxError suggestion paths).  pon has no C
//! tokenizer, so this module unblocks the import chain with the real
//! attribute surface and an honest `NotImplementedError` at instantiation
//! time — loud at the exact call site instead of a silent wrong result.

use std::{mem, ptr, sync::LazyLock};

use super::install_module;
use crate::{
	abi,
	intern::intern,
	object::{PyObject, PyObjectHeader, PyType},
	types::exc::ExceptionKind,
};

/// Instances are never created; the layout exists only to size the type.
#[repr(C)]
struct PyTokenizerIter {
	ob_base: PyObjectHeader,
}

static TOKENIZER_ITER_TYPE: LazyLock<usize> = LazyLock::new(|| {
	let mut ty = PyType::new(
		abi::runtime_type_type().cast_const(),
		"TokenizerIter",
		mem::size_of::<PyTokenizerIter>(),
	);
	ty.tp_base = abi::runtime_global(intern("object"))
		.map_or(ptr::null_mut(), |object| object.cast::<PyType>());
	ty.tp_new = Some(tokenizer_iter_new);
	Box::into_raw(Box::new(ty)) as usize
});

unsafe extern "C" fn tokenizer_iter_new(
	_cls: *mut PyType,
	_args: *mut PyObject,
	_kwargs: *mut PyObject,
) -> *mut PyObject {
	abi::exc::raise_kind_error_text(
		ExceptionKind::NotImplementedError,
		"_tokenize.TokenizerIter is not implemented in pon (no C tokenizer; see native/tokenize.rs)",
	)
}

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	let name = "_tokenize";
	// SAFETY: Runtime allocation helper; NULL is checked below.
	let name_obj = unsafe { abi::pon_const_str(name.as_ptr(), name.len()) };
	if name_obj.is_null() {
		return Err("failed to allocate _tokenize.__name__".to_owned());
	}
	install_module(name, vec![
		(intern("__name__"), name_obj),
		(intern("TokenizerIter"), (*TOKENIZER_ITER_TYPE as *mut PyType).cast::<PyObject>()),
	])
}
