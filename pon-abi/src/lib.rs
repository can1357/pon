//!Stable public ABI façade for Pon runtime embedders and generated code.
//!
//!This crate intentionally contains no runtime implementation. It names the
//!object layouts, helper declarations, GC types, and version constants that
//!external code may depend on while the concrete implementation remains in
//!`pon-runtime` and `pon-gc`.

use core::mem::{offset_of, size_of};

pub use pon_gc::{
	DEFAULT_HEAP_ALIGNMENT, FinalizeFn, GcTypeInfo, PreciseStackRootFn, TraceFn, TypeId as GcTypeId,
};
pub use pon_runtime::{
	abi::{AbiTy, CodeInfo, FStrPartRaw, HandlerInfo, HelperDecl, ParamSpec, PyFrame, TStrPartRaw},
	feedback::{FeedbackCell, FeedbackVec, TypeTag},
	object::{
		BinaryFunc, CallFunc, DescrGetFunc, DescrSetFunc, GcMeta, GetAttrFunc, HashFunc, InitFunc,
		InquiryFunc, LenFunc, NewFunc, ObjObjArgProc, ObjObjProc, PyAsyncMethods, PyDunderSlots,
		PyMappingMethods, PyNumberMethods, PyObject, PyObjectHeader, PySequenceMethods, PyType,
		RichCmpFunc, SSizeArgFunc, SSizeObjArgProc, SendFunc, SetAttrFunc, TernaryFunc, UnaryFunc,
	},
	thread_state::PonThreadState,
};

/// ABI contract major version. Increment this for layout or symbol changes that
/// can break generated code or native embedders compiled against an older
/// façade.
pub const ABI_VERSION_MAJOR: u16 = 0;
/// ABI contract minor version. Increment this for additive symbols/layout
/// fields that older consumers may ignore.
pub const ABI_VERSION_MINOR: u16 = 1;
/// Combined ABI version packed as `major << 16 | minor` for C-facing probes.
pub const ABI_VERSION: u32 = ((ABI_VERSION_MAJOR as u32) << 16) | ABI_VERSION_MINOR as u32;
/// Human-readable ABI family name.
pub const ABI_NAME: &str = "pon-runtime-abi";

/// Every helper exported by `pon-runtime`, in the order used by codegen
/// imports.
pub const RUNTIME_HELPERS: &[HelperDecl] = pon_runtime::abi::HELPERS;

/// Size in bytes of the common object header at the current ABI version.
pub const PY_OBJECT_HEADER_SIZE: usize = size_of::<PyObjectHeader>();
/// Offset of `PyObjectHeader::ob_type`.
pub const PY_OBJECT_HEADER_OB_TYPE_OFFSET: usize = offset_of!(PyObjectHeader, ob_type);
/// Offset of `PyObjectHeader::gc_meta`.
pub const PY_OBJECT_HEADER_GC_META_OFFSET: usize = offset_of!(PyObjectHeader, gc_meta);
/// Size in bytes of the runtime type object layout.
pub const PY_TYPE_SIZE: usize = size_of::<PyType>();
/// Offset of the common object header within `PyType`.
pub const PY_TYPE_OB_BASE_OFFSET: usize = offset_of!(PyType, ob_base);
/// Offset of the type name pointer within `PyType`.
pub const PY_TYPE_NAME_OFFSET: usize = offset_of!(PyType, name);
/// Size in bytes of compiled function metadata.
pub const CODE_INFO_SIZE: usize = size_of::<CodeInfo>();
/// Size in bytes of parameter binding metadata.
pub const PARAM_SPEC_SIZE: usize = size_of::<ParamSpec>();
/// Size in bytes of a frame header used by traceback/generator helpers.
pub const PY_FRAME_SIZE: usize = size_of::<PyFrame>();
/// Size in bytes of one feedback cell.
pub const FEEDBACK_CELL_SIZE: usize = size_of::<FeedbackCell>();
/// Size in bytes of one GC type-info record.
pub const GC_TYPE_INFO_SIZE: usize = size_of::<GcTypeInfo>();

/// Finds a helper declaration by exported symbol name.
#[must_use]
pub fn helper(symbol: &str) -> Option<&'static HelperDecl> {
	RUNTIME_HELPERS.iter().find(|decl| decl.symbol == symbol)
}

/// Returns true when the compiled consumer's ABI version can use this façade.
#[must_use]
pub const fn is_compatible_version(major: u16, minor: u16) -> bool {
	major == ABI_VERSION_MAJOR && minor <= ABI_VERSION_MINOR
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn exposes_runtime_helper_symbols() {
		let helper = helper("pon_const_int").expect("pon_const_int helper");
		assert_eq!(helper.ret, AbiTy::PyObjectPtr);
		assert!(!RUNTIME_HELPERS.is_empty());
	}

	#[test]
	fn layout_constants_match_reexports() {
		assert_eq!(PY_OBJECT_HEADER_SIZE, size_of::<PyObjectHeader>());
		assert_eq!(PY_OBJECT_HEADER_OB_TYPE_OFFSET, 0);
		assert_eq!(PY_TYPE_OB_BASE_OFFSET, 0);
		assert_eq!(GC_TYPE_INFO_SIZE, size_of::<GcTypeInfo>());
	}

	#[test]
	fn version_accepts_same_major_older_minor() {
		assert!(is_compatible_version(ABI_VERSION_MAJOR, 0));
		assert!(!is_compatible_version(ABI_VERSION_MAJOR + 1, 0));
	}
}
