//!Phase-A runtime for boxed Python objects and the compiled-code helper ABI.
#![allow(improper_ctypes_definitions)]

pub mod abi;
pub mod abstract_op;
pub mod aot_entry;
pub mod builtins;
pub mod capi;
pub mod descr;
pub mod dynexec;
pub mod feedback;
pub(crate) mod gcroot;
pub mod import;
pub mod inspect;
pub mod intern;
pub mod mro;
pub mod native;
pub mod object;
pub mod pyhash;
pub mod stackmap;
pub mod sync;
pub mod sys;
pub mod tag;
pub mod thread;
pub mod thread_state;
pub(crate) mod traceback;
pub mod types;

pub use abi::*;
pub use feedback::*;
pub use intern::{intern, resolve};
/// Drains pending OS signals for generated-code poll bodies.
pub unsafe extern "C" fn pon_signal_check_pending() -> libc::c_int {
	unsafe { native::signal::pon_signal_check_pending() }
}
pub use object::*;
pub use stackmap::*;
pub use sync::*;
pub use thread::*;
pub use thread_state::*;
