//! Phase-B per-type module hub.
//!
//! Each concrete Python type lives in a family-owned module so Phase-B
//! workstreams can fill behavior without contending on the hub.

/// Slot-layout re-exports shared by per-type implementations.
pub mod slots {
    pub use crate::object::{
        BinaryFunc, CallFunc, DescrGetFunc, DescrSetFunc, GetAttrFunc, HashFunc, InitFunc, InquiryFunc, LenFunc,
        NewFunc, ObjObjArgProc, ObjObjProc, PyAsyncMethods, PyDunderSlots, PyMappingMethods, PyNumberMethods, PySequenceMethods,
        RichCmpFunc, SSizeArgFunc, SSizeObjArgProc, SendFunc, SetAttrFunc, TernaryFunc, UnaryFunc,
    };
}

/// Exception hierarchy and boxed exception payloads.
pub mod exc;
pub mod weakref;

/// Numeric type modules.
pub mod int;
pub mod float;
pub mod bool_;
pub mod complex_;

/// Text and binary type modules.
pub mod str_;
pub mod bytes_;
pub mod bytearray_;
pub mod memoryview;

/// Sequence type modules.
pub mod list;
pub mod tuple;
pub mod range_;
pub mod slice_;
pub mod lazy_iter;

/// Mapping and set type modules.
pub mod dict;
pub mod set_;
pub mod frozenset;

/// Callable and closure type modules.
pub mod function;
pub mod cell;
pub mod method;

/// Class/data-model type modules.
pub mod type_;
pub mod property;
pub mod classmethod;
pub mod super_;

/// Generator/coroutine/frame type modules.
pub mod generator;
pub mod coroutine;
pub mod async_generator;
pub mod frame;

/// Import and typing support modules.
pub mod module;
pub mod typealias;

/// Phase-A compatibility namespaces retained until concrete modules absorb them.
pub mod long;
pub mod unicode;
pub mod none;
