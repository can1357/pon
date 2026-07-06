//! Phase-B per-type module hub.
//!
//! Each concrete Python type lives in a family-owned module so Phase-B
//! workstreams can fill behavior without contending on the hub.

/// Slot-layout re-exports shared by per-type implementations.
pub mod slots {
	pub use crate::object::{
		BinaryFunc, CallFunc, DescrGetFunc, DescrSetFunc, GetAttrFunc, HashFunc, InitFunc,
		InquiryFunc, LenFunc, NewFunc, ObjObjArgProc, ObjObjProc, PyAsyncMethods, PyDunderSlots,
		PyMappingMethods, PyNumberMethods, PySequenceMethods, RichCmpFunc, SSizeArgFunc,
		SSizeObjArgProc, SendFunc, SetAttrFunc, TernaryFunc, UnaryFunc,
	};
}

/// Exception hierarchy and boxed exception payloads.
pub mod exc;
pub mod weakref;

pub mod bool_;
pub mod complex_;
pub mod float;
/// Numeric type modules.
pub mod int;

pub mod bytearray_;
pub mod bytes_;
pub mod memoryview;
/// Text and binary type modules.
pub mod str_;

pub mod lazy_iter;
/// Sequence type modules.
pub mod list;
pub mod range_;
pub mod slice_;
pub mod tuple;

/// Mapping and set type modules.
pub mod dict;
pub mod frozenset;
pub mod instance_dict;
pub mod set_;

pub mod cell;
/// Callable and closure type modules.
pub mod function;
pub mod method;

pub mod classmethod;
pub mod property;
pub mod super_;
/// Class/data-model type modules.
pub mod type_;

pub mod async_generator;
pub mod coroutine;
pub mod frame;
/// Generator/coroutine/frame type modules.
pub mod generator;

/// Import and typing support modules.
pub mod module;
pub mod typealias;

/// Phase-A compatibility namespaces retained until concrete modules absorb
/// them.
pub mod long;
pub mod none;
pub mod unicode;
