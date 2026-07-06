//! Boxed exception objects and the Phase-B builtin exception type hierarchy.
//!
//! Exception instances are ordinary boxed Python objects with no refcount
//! field. The runtime owns allocation through `pon-gc`; this module only
//! defines the layout, immortal type descriptors, and hierarchy queries shared
//! by ABI helpers.

use core::{
	ffi::c_int,
	mem::{offset_of, size_of},
	ptr,
};
use std::sync::LazyLock;

use crate::object::{PyObject, PyObjectHeader, PyType, as_object_ptr};

/// Minimal boxed exception payload shared by every builtin exception class.
#[repr(C)]
#[derive(Debug)]
pub struct PyBaseException {
	/// Common object header; this field must remain first.
	pub ob_base:          PyObjectHeader,
	/// Boxed message/value payload.  Message-raising helpers store `str`; value
	/// exceptions such as `KeyError` and `StopIteration` store the carried
	/// value.
	pub message:          *mut PyObject,
	/// Explicit exception cause (`raise ... from ...`), or NULL.
	pub cause:            *mut PyObject,
	/// Implicit exception context, or NULL.
	pub context:          *mut PyObject,
	/// Traceback object slot reserved for the traceback workstream, or NULL.
	pub traceback:        *mut PyObject,
	/// Full positional-argument tuple when the constructor received two or
	/// more arguments; NULL when `args` derives from `message` (zero or one
	/// argument), keeping single-value native raise paths allocation-free.
	pub args:             *mut PyObject,
	/// Lazily created per-instance attribute dictionary, or NULL.
	pub dict:             *mut crate::types::type_::PyClassDict,
	/// `__suppress_context__`: set by an explicit `raise ... from ...` (PEP
	/// 3134).
	pub suppress_context: bool,
}

impl PyBaseException {
	/// Builds an exception object payload for `ty`.
	#[must_use]
	pub const fn new(
		ty: *const PyType,
		message: *mut PyObject,
		cause: *mut PyObject,
		context: *mut PyObject,
		traceback: *mut PyObject,
	) -> Self {
		Self {
			ob_base: PyObjectHeader::new(ty),
			message,
			cause,
			context,
			traceback,
			args: ptr::null_mut(),
			dict: ptr::null_mut(),
			suppress_context: false,
		}
	}
}

/// Boxed exception-group payload: a BaseException plus its immutable member
/// tuple.
#[repr(C)]
#[derive(Debug)]
pub struct PyExceptionGroup {
	/// Common exception payload; must remain first.
	pub base:       PyBaseException,
	/// Boxed tuple of member exceptions. Non-NULL for valid groups.
	pub exceptions: *mut PyObject,
}

#[repr(C)]
#[derive(Debug)]
pub struct PyExceptionGroupMethod {
	pub ob_base:  PyObjectHeader,
	pub receiver: *mut PyObject,
	pub kind:     u8,
}

pub const EXC_GROUP_METHOD_SPLIT: u8 = 0;
pub const EXC_GROUP_METHOD_SUBGROUP: u8 = 1;
pub const EXC_GROUP_METHOD_DERIVE: u8 = 2;

/// Builtin exception class selector used by raising helpers and tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExceptionKind {
	BaseException,
	BaseExceptionGroup,
	GeneratorExit,
	KeyboardInterrupt,
	SystemExit,
	Exception,
	ArithmeticError,
	FloatingPointError,
	OverflowError,
	ZeroDivisionError,
	AssertionError,
	AttributeError,
	BufferError,
	EOFError,
	ImportError,
	ModuleNotFoundError,
	LookupError,
	IndexError,
	KeyError,
	MemoryError,
	NameError,
	UnboundLocalError,
	OSError,
	BlockingIOError,
	ChildProcessError,
	ConnectionError,
	BrokenPipeError,
	ConnectionAbortedError,
	ConnectionRefusedError,
	ConnectionResetError,
	FileExistsError,
	FileNotFoundError,
	InterruptedError,
	IsADirectoryError,
	NotADirectoryError,
	PermissionError,
	ProcessLookupError,
	TimeoutError,
	ReferenceError,
	RuntimeError,
	NotImplementedError,
	PythonFinalizationError,
	RecursionError,
	StopAsyncIteration,
	StopIteration,
	SyntaxError,
	IndentationError,
	TabError,
	SystemError,
	TypeError,
	ValueError,
	UnicodeError,
	UnicodeDecodeError,
	UnicodeEncodeError,
	UnicodeTranslateError,
	Warning,
	BytesWarning,
	DeprecationWarning,
	EncodingWarning,
	FutureWarning,
	ImportWarning,
	PendingDeprecationWarning,
	ResourceWarning,
	RuntimeWarning,
	SyntaxWarning,
	UnicodeWarning,
	UserWarning,
	ExceptionGroup,
}

/// Immortal builtin exception type descriptors created during runtime init.
#[derive(Clone, Copy, Debug)]
pub struct ExceptionTypeSet {
	pub base_exception:              *mut PyType,
	pub base_exception_group:        *mut PyType,
	pub generator_exit:              *mut PyType,
	pub keyboard_interrupt:          *mut PyType,
	pub system_exit:                 *mut PyType,
	pub exception:                   *mut PyType,
	pub arithmetic_error:            *mut PyType,
	pub floating_point_error:        *mut PyType,
	pub overflow_error:              *mut PyType,
	pub zero_division_error:         *mut PyType,
	pub assertion_error:             *mut PyType,
	pub attribute_error:             *mut PyType,
	pub buffer_error:                *mut PyType,
	pub eof_error:                   *mut PyType,
	pub import_error:                *mut PyType,
	pub module_not_found_error:      *mut PyType,
	pub lookup_error:                *mut PyType,
	pub index_error:                 *mut PyType,
	pub key_error:                   *mut PyType,
	pub memory_error:                *mut PyType,
	pub name_error:                  *mut PyType,
	pub unbound_local_error:         *mut PyType,
	pub os_error:                    *mut PyType,
	pub blocking_io_error:           *mut PyType,
	pub child_process_error:         *mut PyType,
	pub connection_error:            *mut PyType,
	pub broken_pipe_error:           *mut PyType,
	pub connection_aborted_error:    *mut PyType,
	pub connection_refused_error:    *mut PyType,
	pub connection_reset_error:      *mut PyType,
	pub file_exists_error:           *mut PyType,
	pub file_not_found_error:        *mut PyType,
	pub interrupted_error:           *mut PyType,
	pub is_a_directory_error:        *mut PyType,
	pub not_a_directory_error:       *mut PyType,
	pub permission_error:            *mut PyType,
	pub process_lookup_error:        *mut PyType,
	pub timeout_error:               *mut PyType,
	pub reference_error:             *mut PyType,
	pub runtime_error:               *mut PyType,
	pub not_implemented_error:       *mut PyType,
	pub python_finalization_error:   *mut PyType,
	pub recursion_error:             *mut PyType,
	pub stop_async_iteration:        *mut PyType,
	pub stop_iteration:              *mut PyType,
	pub syntax_error:                *mut PyType,
	pub indentation_error:           *mut PyType,
	pub tab_error:                   *mut PyType,
	pub system_error:                *mut PyType,
	pub type_error:                  *mut PyType,
	pub value_error:                 *mut PyType,
	pub unicode_error:               *mut PyType,
	pub unicode_decode_error:        *mut PyType,
	pub unicode_encode_error:        *mut PyType,
	pub unicode_translate_error:     *mut PyType,
	pub warning:                     *mut PyType,
	pub bytes_warning:               *mut PyType,
	pub deprecation_warning:         *mut PyType,
	pub encoding_warning:            *mut PyType,
	pub future_warning:              *mut PyType,
	pub import_warning:              *mut PyType,
	pub pending_deprecation_warning: *mut PyType,
	pub resource_warning:            *mut PyType,
	pub runtime_warning:             *mut PyType,
	pub syntax_warning:              *mut PyType,
	pub unicode_warning:             *mut PyType,
	pub user_warning:                *mut PyType,
	pub exception_group:             *mut PyType,
}

impl ExceptionTypeSet {
	/// Creates the builtin hierarchy rooted at `BaseException`.
	#[must_use]
	pub fn new(type_type: *mut PyType) -> Self {
		let base_exception = new_exception_type(type_type, "BaseException", ptr::null_mut());
		let base_exception_group =
			new_exception_group_type(type_type, "BaseExceptionGroup", base_exception);
		let generator_exit = new_exception_type(type_type, "GeneratorExit", base_exception);
		let keyboard_interrupt = new_exception_type(type_type, "KeyboardInterrupt", base_exception);
		let system_exit = new_exception_type(type_type, "SystemExit", base_exception);
		let exception = new_exception_type(type_type, "Exception", base_exception);
		let arithmetic_error = new_exception_type(type_type, "ArithmeticError", exception);
		let floating_point_error =
			new_exception_type(type_type, "FloatingPointError", arithmetic_error);
		let overflow_error = new_exception_type(type_type, "OverflowError", arithmetic_error);
		let zero_division_error =
			new_exception_type(type_type, "ZeroDivisionError", arithmetic_error);
		let assertion_error = new_exception_type(type_type, "AssertionError", exception);
		let attribute_error = new_exception_type(type_type, "AttributeError", exception);
		let buffer_error = new_exception_type(type_type, "BufferError", exception);
		let eof_error = new_exception_type(type_type, "EOFError", exception);
		let import_error = new_exception_type(type_type, "ImportError", exception);
		let module_not_found_error =
			new_exception_type(type_type, "ModuleNotFoundError", import_error);
		let lookup_error = new_exception_type(type_type, "LookupError", exception);
		let index_error = new_exception_type(type_type, "IndexError", lookup_error);
		let key_error = new_exception_type(type_type, "KeyError", lookup_error);
		let memory_error = new_exception_type(type_type, "MemoryError", exception);
		let name_error = new_exception_type(type_type, "NameError", exception);
		let unbound_local_error = new_exception_type(type_type, "UnboundLocalError", name_error);
		let os_error = new_exception_type(type_type, "OSError", exception);
		let blocking_io_error = new_exception_type(type_type, "BlockingIOError", os_error);
		let child_process_error = new_exception_type(type_type, "ChildProcessError", os_error);
		let connection_error = new_exception_type(type_type, "ConnectionError", os_error);
		let broken_pipe_error = new_exception_type(type_type, "BrokenPipeError", connection_error);
		let connection_aborted_error =
			new_exception_type(type_type, "ConnectionAbortedError", connection_error);
		let connection_refused_error =
			new_exception_type(type_type, "ConnectionRefusedError", connection_error);
		let connection_reset_error =
			new_exception_type(type_type, "ConnectionResetError", connection_error);
		let file_exists_error = new_exception_type(type_type, "FileExistsError", os_error);
		let file_not_found_error = new_exception_type(type_type, "FileNotFoundError", os_error);
		let interrupted_error = new_exception_type(type_type, "InterruptedError", os_error);
		let is_a_directory_error = new_exception_type(type_type, "IsADirectoryError", os_error);
		let not_a_directory_error = new_exception_type(type_type, "NotADirectoryError", os_error);
		let permission_error = new_exception_type(type_type, "PermissionError", os_error);
		let process_lookup_error = new_exception_type(type_type, "ProcessLookupError", os_error);
		let timeout_error = new_exception_type(type_type, "TimeoutError", os_error);
		let reference_error = new_exception_type(type_type, "ReferenceError", exception);
		let runtime_error = new_exception_type(type_type, "RuntimeError", exception);
		let not_implemented_error =
			new_exception_type(type_type, "NotImplementedError", runtime_error);
		let python_finalization_error =
			new_exception_type(type_type, "PythonFinalizationError", runtime_error);
		let recursion_error = new_exception_type(type_type, "RecursionError", runtime_error);
		let stop_async_iteration = new_exception_type(type_type, "StopAsyncIteration", exception);
		let stop_iteration = new_exception_type(type_type, "StopIteration", exception);
		let syntax_error = new_exception_type(type_type, "SyntaxError", exception);
		let indentation_error = new_exception_type(type_type, "IndentationError", syntax_error);
		let tab_error = new_exception_type(type_type, "TabError", indentation_error);
		let system_error = new_exception_type(type_type, "SystemError", exception);
		let type_error = new_exception_type(type_type, "TypeError", exception);
		let value_error = new_exception_type(type_type, "ValueError", exception);
		let unicode_error = new_exception_type(type_type, "UnicodeError", value_error);
		let unicode_decode_error = new_exception_type(type_type, "UnicodeDecodeError", unicode_error);
		let unicode_encode_error = new_exception_type(type_type, "UnicodeEncodeError", unicode_error);
		let unicode_translate_error =
			new_exception_type(type_type, "UnicodeTranslateError", unicode_error);
		let warning = new_exception_type(type_type, "Warning", exception);
		let bytes_warning = new_exception_type(type_type, "BytesWarning", warning);
		let deprecation_warning = new_exception_type(type_type, "DeprecationWarning", warning);
		let encoding_warning = new_exception_type(type_type, "EncodingWarning", warning);
		let future_warning = new_exception_type(type_type, "FutureWarning", warning);
		let import_warning = new_exception_type(type_type, "ImportWarning", warning);
		let pending_deprecation_warning =
			new_exception_type(type_type, "PendingDeprecationWarning", warning);
		let resource_warning = new_exception_type(type_type, "ResourceWarning", warning);
		let runtime_warning = new_exception_type(type_type, "RuntimeWarning", warning);
		let syntax_warning = new_exception_type(type_type, "SyntaxWarning", warning);
		let unicode_warning = new_exception_type(type_type, "UnicodeWarning", warning);
		let user_warning = new_exception_type(type_type, "UserWarning", warning);
		let exception_group =
			new_exception_group_type(type_type, "ExceptionGroup", base_exception_group);

		Self {
			base_exception,
			base_exception_group,
			generator_exit,
			keyboard_interrupt,
			system_exit,
			exception,
			arithmetic_error,
			floating_point_error,
			overflow_error,
			zero_division_error,
			assertion_error,
			attribute_error,
			buffer_error,
			eof_error,
			import_error,
			module_not_found_error,
			lookup_error,
			index_error,
			key_error,
			memory_error,
			name_error,
			unbound_local_error,
			os_error,
			blocking_io_error,
			child_process_error,
			connection_error,
			broken_pipe_error,
			connection_aborted_error,
			connection_refused_error,
			connection_reset_error,
			file_exists_error,
			file_not_found_error,
			interrupted_error,
			is_a_directory_error,
			not_a_directory_error,
			permission_error,
			process_lookup_error,
			timeout_error,
			reference_error,
			runtime_error,
			not_implemented_error,
			python_finalization_error,
			recursion_error,
			stop_async_iteration,
			stop_iteration,
			syntax_error,
			indentation_error,
			tab_error,
			system_error,
			type_error,
			value_error,
			unicode_error,
			unicode_decode_error,
			unicode_encode_error,
			unicode_translate_error,
			warning,
			bytes_warning,
			deprecation_warning,
			encoding_warning,
			future_warning,
			import_warning,
			pending_deprecation_warning,
			resource_warning,
			runtime_warning,
			syntax_warning,
			unicode_warning,
			user_warning,
			exception_group,
		}
	}

	/// Returns the type descriptor for a builtin exception selector.
	#[must_use]
	pub fn get(self, kind: ExceptionKind) -> *mut PyType {
		match kind {
			ExceptionKind::BaseException => self.base_exception,
			ExceptionKind::BaseExceptionGroup => self.base_exception_group,
			ExceptionKind::GeneratorExit => self.generator_exit,
			ExceptionKind::KeyboardInterrupt => self.keyboard_interrupt,
			ExceptionKind::SystemExit => self.system_exit,
			ExceptionKind::Exception => self.exception,
			ExceptionKind::ArithmeticError => self.arithmetic_error,
			ExceptionKind::FloatingPointError => self.floating_point_error,
			ExceptionKind::OverflowError => self.overflow_error,
			ExceptionKind::ZeroDivisionError => self.zero_division_error,
			ExceptionKind::AssertionError => self.assertion_error,
			ExceptionKind::AttributeError => self.attribute_error,
			ExceptionKind::BufferError => self.buffer_error,
			ExceptionKind::EOFError => self.eof_error,
			ExceptionKind::ImportError => self.import_error,
			ExceptionKind::ModuleNotFoundError => self.module_not_found_error,
			ExceptionKind::LookupError => self.lookup_error,
			ExceptionKind::IndexError => self.index_error,
			ExceptionKind::KeyError => self.key_error,
			ExceptionKind::MemoryError => self.memory_error,
			ExceptionKind::NameError => self.name_error,
			ExceptionKind::UnboundLocalError => self.unbound_local_error,
			ExceptionKind::OSError => self.os_error,
			ExceptionKind::BlockingIOError => self.blocking_io_error,
			ExceptionKind::ChildProcessError => self.child_process_error,
			ExceptionKind::ConnectionError => self.connection_error,
			ExceptionKind::BrokenPipeError => self.broken_pipe_error,
			ExceptionKind::ConnectionAbortedError => self.connection_aborted_error,
			ExceptionKind::ConnectionRefusedError => self.connection_refused_error,
			ExceptionKind::ConnectionResetError => self.connection_reset_error,
			ExceptionKind::FileExistsError => self.file_exists_error,
			ExceptionKind::FileNotFoundError => self.file_not_found_error,
			ExceptionKind::InterruptedError => self.interrupted_error,
			ExceptionKind::IsADirectoryError => self.is_a_directory_error,
			ExceptionKind::NotADirectoryError => self.not_a_directory_error,
			ExceptionKind::PermissionError => self.permission_error,
			ExceptionKind::ProcessLookupError => self.process_lookup_error,
			ExceptionKind::TimeoutError => self.timeout_error,
			ExceptionKind::ReferenceError => self.reference_error,
			ExceptionKind::RuntimeError => self.runtime_error,
			ExceptionKind::NotImplementedError => self.not_implemented_error,
			ExceptionKind::PythonFinalizationError => self.python_finalization_error,
			ExceptionKind::RecursionError => self.recursion_error,
			ExceptionKind::StopAsyncIteration => self.stop_async_iteration,
			ExceptionKind::StopIteration => self.stop_iteration,
			ExceptionKind::SyntaxError => self.syntax_error,
			ExceptionKind::IndentationError => self.indentation_error,
			ExceptionKind::TabError => self.tab_error,
			ExceptionKind::SystemError => self.system_error,
			ExceptionKind::TypeError => self.type_error,
			ExceptionKind::ValueError => self.value_error,
			ExceptionKind::UnicodeError => self.unicode_error,
			ExceptionKind::UnicodeDecodeError => self.unicode_decode_error,
			ExceptionKind::UnicodeEncodeError => self.unicode_encode_error,
			ExceptionKind::UnicodeTranslateError => self.unicode_translate_error,
			ExceptionKind::Warning => self.warning,
			ExceptionKind::BytesWarning => self.bytes_warning,
			ExceptionKind::DeprecationWarning => self.deprecation_warning,
			ExceptionKind::EncodingWarning => self.encoding_warning,
			ExceptionKind::FutureWarning => self.future_warning,
			ExceptionKind::ImportWarning => self.import_warning,
			ExceptionKind::PendingDeprecationWarning => self.pending_deprecation_warning,
			ExceptionKind::ResourceWarning => self.resource_warning,
			ExceptionKind::RuntimeWarning => self.runtime_warning,
			ExceptionKind::SyntaxWarning => self.syntax_warning,
			ExceptionKind::UnicodeWarning => self.unicode_warning,
			ExceptionKind::UserWarning => self.user_warning,
			ExceptionKind::ExceptionGroup => self.exception_group,
		}
	}

	/// Returns every core builtin exception type required by B05-EXC-CORE and
	/// wave-2 compat.
	#[must_use]
	pub fn core_types(self) -> [(ExceptionKind, *mut PyType); 68] {
		[
			(ExceptionKind::BaseException, self.base_exception),
			(ExceptionKind::BaseExceptionGroup, self.base_exception_group),
			(ExceptionKind::GeneratorExit, self.generator_exit),
			(ExceptionKind::KeyboardInterrupt, self.keyboard_interrupt),
			(ExceptionKind::SystemExit, self.system_exit),
			(ExceptionKind::Exception, self.exception),
			(ExceptionKind::ArithmeticError, self.arithmetic_error),
			(ExceptionKind::FloatingPointError, self.floating_point_error),
			(ExceptionKind::OverflowError, self.overflow_error),
			(ExceptionKind::ZeroDivisionError, self.zero_division_error),
			(ExceptionKind::AssertionError, self.assertion_error),
			(ExceptionKind::AttributeError, self.attribute_error),
			(ExceptionKind::BufferError, self.buffer_error),
			(ExceptionKind::EOFError, self.eof_error),
			(ExceptionKind::ImportError, self.import_error),
			(ExceptionKind::ModuleNotFoundError, self.module_not_found_error),
			(ExceptionKind::LookupError, self.lookup_error),
			(ExceptionKind::IndexError, self.index_error),
			(ExceptionKind::KeyError, self.key_error),
			(ExceptionKind::MemoryError, self.memory_error),
			(ExceptionKind::NameError, self.name_error),
			(ExceptionKind::UnboundLocalError, self.unbound_local_error),
			(ExceptionKind::OSError, self.os_error),
			(ExceptionKind::BlockingIOError, self.blocking_io_error),
			(ExceptionKind::ChildProcessError, self.child_process_error),
			(ExceptionKind::ConnectionError, self.connection_error),
			(ExceptionKind::BrokenPipeError, self.broken_pipe_error),
			(ExceptionKind::ConnectionAbortedError, self.connection_aborted_error),
			(ExceptionKind::ConnectionRefusedError, self.connection_refused_error),
			(ExceptionKind::ConnectionResetError, self.connection_reset_error),
			(ExceptionKind::FileExistsError, self.file_exists_error),
			(ExceptionKind::FileNotFoundError, self.file_not_found_error),
			(ExceptionKind::InterruptedError, self.interrupted_error),
			(ExceptionKind::IsADirectoryError, self.is_a_directory_error),
			(ExceptionKind::NotADirectoryError, self.not_a_directory_error),
			(ExceptionKind::PermissionError, self.permission_error),
			(ExceptionKind::ProcessLookupError, self.process_lookup_error),
			(ExceptionKind::TimeoutError, self.timeout_error),
			(ExceptionKind::ReferenceError, self.reference_error),
			(ExceptionKind::RuntimeError, self.runtime_error),
			(ExceptionKind::NotImplementedError, self.not_implemented_error),
			(ExceptionKind::PythonFinalizationError, self.python_finalization_error),
			(ExceptionKind::RecursionError, self.recursion_error),
			(ExceptionKind::StopAsyncIteration, self.stop_async_iteration),
			(ExceptionKind::StopIteration, self.stop_iteration),
			(ExceptionKind::SyntaxError, self.syntax_error),
			(ExceptionKind::IndentationError, self.indentation_error),
			(ExceptionKind::TabError, self.tab_error),
			(ExceptionKind::SystemError, self.system_error),
			(ExceptionKind::TypeError, self.type_error),
			(ExceptionKind::ValueError, self.value_error),
			(ExceptionKind::UnicodeError, self.unicode_error),
			(ExceptionKind::UnicodeDecodeError, self.unicode_decode_error),
			(ExceptionKind::UnicodeEncodeError, self.unicode_encode_error),
			(ExceptionKind::UnicodeTranslateError, self.unicode_translate_error),
			(ExceptionKind::Warning, self.warning),
			(ExceptionKind::BytesWarning, self.bytes_warning),
			(ExceptionKind::DeprecationWarning, self.deprecation_warning),
			(ExceptionKind::EncodingWarning, self.encoding_warning),
			(ExceptionKind::FutureWarning, self.future_warning),
			(ExceptionKind::ImportWarning, self.import_warning),
			(ExceptionKind::PendingDeprecationWarning, self.pending_deprecation_warning),
			(ExceptionKind::ResourceWarning, self.resource_warning),
			(ExceptionKind::RuntimeWarning, self.runtime_warning),
			(ExceptionKind::SyntaxWarning, self.syntax_warning),
			(ExceptionKind::UnicodeWarning, self.unicode_warning),
			(ExceptionKind::UserWarning, self.user_warning),
			(ExceptionKind::ExceptionGroup, self.exception_group),
		]
	}

	/// Returns true when `ty` is `BaseExceptionGroup`/`ExceptionGroup` or a
	/// subclass.
	#[must_use]
	pub unsafe fn is_exception_group_type(self, ty: *const PyType) -> bool {
		// SAFETY: Delegates to hierarchy traversal with the same caller contract.
		unsafe { is_exception_subclass(ty, self.base_exception_group.cast_const()) }
	}
}

fn new_exception_type(
	type_type: *mut PyType,
	name: &'static str,
	base: *mut PyType,
) -> *mut PyType {
	let mut ty = PyType::new(type_type.cast_const(), name, size_of::<PyBaseException>());
	ty.tp_base = base;
	ty.tp_getattro = Some(exception_getattro);
	ty.tp_setattro = Some(exception_setattro);
	Box::into_raw(Box::new(ty))
}

fn new_exception_group_type(
	type_type: *mut PyType,
	name: &'static str,
	base: *mut PyType,
) -> *mut PyType {
	let mut ty = PyType::new(type_type.cast_const(), name, size_of::<PyExceptionGroup>());
	ty.tp_base = base;
	ty.tp_getattro = Some(exception_getattro);
	ty.tp_setattro = Some(exception_setattro);
	Box::into_raw(Box::new(ty))
}

fn exception_group_method_type() -> *mut PyType {
	static TYPE: LazyLock<usize> = LazyLock::new(|| {
		let mut ty =
			PyType::new(ptr::null(), "exception_group_method", size_of::<PyExceptionGroupMethod>());
		ty.tp_call = Some(exception_group_method_call);
		Box::into_raw(Box::new(ty)) as usize
	});
	*TYPE as *mut PyType
}

#[must_use]
pub fn new_exception_group_method(receiver: *mut PyObject, kind: u8) -> *mut PyObject {
	Box::into_raw(Box::new(PyExceptionGroupMethod {
		ob_base: PyObjectHeader::new(exception_group_method_type()),
		receiver,
		kind,
	}))
	.cast::<PyObject>()
}

unsafe extern "C" fn exception_group_method_call(
	callee: *mut PyObject,
	args: *mut PyObject,
	_kwargs: *mut PyObject,
) -> *mut PyObject {
	if callee.is_null() {
		crate::thread_state::pon_err_set("exception group method receiver is NULL");
		return ptr::null_mut();
	}
	let method = unsafe { &*callee.cast::<PyExceptionGroupMethod>() };
	unsafe { crate::abi::exc::call_exception_group_method(method.receiver, method.kind, args) }
}

#[must_use]
pub unsafe fn is_exception_group_type_ptr(mut ty: *const PyType) -> bool {
	while !ty.is_null() {
		let name = unsafe { (*ty).name() };
		if name == "BaseExceptionGroup" || name == "ExceptionGroup" {
			return true;
		}
		ty = unsafe { (*ty).tp_base.cast_const() };
	}
	false
}

#[must_use]
pub unsafe fn is_exception_group_instance(object: *mut PyObject) -> bool {
	!object.is_null() && unsafe { is_exception_group_type_ptr((*object).ob_type) }
}

#[must_use]
pub unsafe fn as_exception_group<'a>(object: *mut PyObject) -> Option<&'a PyExceptionGroup> {
	if unsafe { is_exception_group_instance(object) } {
		Some(unsafe { &*object.cast::<PyExceptionGroup>() })
	} else {
		None
	}
}

/// `tp_getattro` for boxed exception instances — builtin classes AND
/// Python-defined subclasses (both share the `PyBaseException` layout).
///
/// CPython resolution order with the builtin exception surface acting as
/// `BaseException`'s own C-level descriptors at the MRO tail: class data
/// descriptors win, then the fixed field surface (unless shadowed by ANY
/// class-namespace hit above `BaseException`), then the instance dict, then
/// non-data class attributes, then the synthesized `BaseException` methods.
pub(crate) unsafe extern "C" fn exception_getattro(
	object: *mut PyObject,
	name: *mut PyObject,
) -> *mut PyObject {
	let Some(name) = (unsafe { crate::types::type_::unicode_text(name) }) else {
		crate::thread_state::pon_err_set("exception attribute name must be str");
		return ptr::null_mut();
	};
	let name_id = crate::intern::intern(name);
	let obj_ty = unsafe { (*object).ob_type.cast_mut() };
	let class_descr = unsafe { crate::descr::lookup_in_type(obj_ty, name_id) };
	if unsafe { crate::descr::is_data_descriptor(class_descr) } {
		return unsafe { crate::descr::descriptor_get(class_descr, object, obj_ty) };
	}
	if class_descr.is_null() {
		if let Some(value) = unsafe { exception_fixed_attr(object, name) } {
			return value;
		}
	}
	let exception = unsafe { &*object.cast::<PyBaseException>() };
	if !exception.dict.is_null() {
		if let Some(value) = unsafe { (&*exception.dict).get(name_id) } {
			return value;
		}
	}
	if !class_descr.is_null() {
		return unsafe { crate::descr::descriptor_get(class_descr, object, obj_ty) };
	}
	if let Some(method) = unsafe { exception_synth_method(object, name) } {
		return method;
	}
	unsafe { crate::abi::pon_raise_attribute_error(object, name_id) }
}

/// Serves the fixed `BaseException` instance surface (CPython's C-level
/// getsets/members): `Some` when `name` belongs to the surface, `None` to let
/// the caller continue the generic resolution order.
unsafe fn exception_fixed_attr(object: *mut PyObject, name: &str) -> Option<*mut PyObject> {
	let exception = unsafe { &*object.cast::<PyBaseException>() };
	let is_group = unsafe { is_exception_group_instance(object) };
	match name {
		"args" => Some(if !exception.args.is_null() {
			exception.args
		} else if is_group {
			let group = unsafe { &*object.cast::<PyExceptionGroup>() };
			crate::native::builtins_mod::alloc_tuple(vec![exception.message, group.exceptions])
		} else if exception.message.is_null() {
			crate::native::builtins_mod::alloc_tuple(Vec::new())
		} else {
			crate::native::builtins_mod::alloc_tuple(vec![exception.message])
		}),
		"message" => Some(if exception.message.is_null() {
			unsafe { crate::abi::pon_none() }
		} else {
			exception.message
		}),
		"exceptions" if is_group => Some(unsafe { (&*object.cast::<PyExceptionGroup>()).exceptions }),
		"split" if is_group => Some(new_exception_group_method(object, EXC_GROUP_METHOD_SPLIT)),
		"subgroup" if is_group => Some(new_exception_group_method(object, EXC_GROUP_METHOD_SUBGROUP)),
		"derive" if is_group => Some(new_exception_group_method(object, EXC_GROUP_METHOD_DERIVE)),
		"value" if unsafe { exception_type_named((*object).ob_type, "StopIteration") } => {
			Some(if exception.message.is_null() {
				unsafe { crate::abi::pon_none() }
			} else {
				exception.message
			})
		},
		// CPython `ImportError`'s C members (`name`, `path`, `name_from`):
		// constructor keywords land in the instance dict, and unset reads
		// default to None instead of AttributeError.  (pon keeps them
		// dict-backed, so unlike CPython they are `__dict__`-visible.)
		"name" | "path" | "name_from"
			if unsafe { exception_type_named((*object).ob_type, "ImportError") } =>
		{
			let stored = if exception.dict.is_null() {
				None
			} else {
				unsafe { (&*exception.dict).get(crate::intern::intern(name)) }
			};
			Some(stored.unwrap_or_else(|| unsafe { crate::abi::pon_none() }))
		},
		// CPython `OSError`'s fixed C members are projections of the
		// positional args tuple (`errno`, `strerror`, `filename`,
		// `filename2`). Single-argument `OSError("msg")` leaves them unset,
		// so the fixed surface answers `None` instead of AttributeError.
		"errno" | "strerror" | "filename" | "filename2"
			if unsafe { exception_type_named((*object).ob_type, "OSError") } =>
		{
			Some(unsafe { os_error_fixed_attr(exception, name) })
		},
		"__cause__" => Some(if exception.cause.is_null() {
			unsafe { crate::abi::pon_none() }
		} else {
			exception.cause
		}),
		"__context__" => Some(if exception.context.is_null() {
			unsafe { crate::abi::pon_none() }
		} else {
			exception.context
		}),
		"__suppress_context__" => Some(crate::types::bool_::from_bool(exception.suppress_context)),
		"__traceback__" => Some(if exception.traceback.is_null() {
			unsafe { crate::abi::pon_none() }
		} else {
			exception.traceback
		}),
		"__class__" => Some(unsafe { (*object).ob_type.cast_mut() }.cast::<PyObject>()),
		"__dict__" => {
			let exception = unsafe { &mut *object.cast::<PyBaseException>() };
			let dict = unsafe { ensure_exception_dict(exception) };
			Some(unsafe { crate::descr::class_dict_to_dict(dict) })
		},
		_ => None,
	}
}
/// Serves CPython's fixed `OSError` members from the constructor args tuple.
unsafe fn os_error_fixed_attr(exception: &PyBaseException, name: &str) -> *mut PyObject {
	let none = unsafe { crate::abi::pon_none() };
	let Some(items) = (!exception.args.is_null())
		.then(|| unsafe { (*exception.args.cast::<crate::types::tuple::PyTuple>()).as_slice() })
	else {
		return none;
	};
	match name {
		"errno" => items.first().copied(),
		"strerror" => items.get(1).copied(),
		"filename" => items.get(2).copied(),
		"filename2" => items.get(4).copied(),
		_ => None,
	}
	.unwrap_or(none)
}

/// Returns the instance dict, creating it on first use.
unsafe fn ensure_exception_dict(
	exception: &mut PyBaseException,
) -> *mut crate::types::type_::PyClassDict {
	if exception.dict.is_null() {
		exception.dict = crate::types::type_::new_namespace();
	}
	exception.dict
}

/// Stores a constructor-bound attribute in the instance dict (the ImportError
/// family's `name=`/`path=`/`name_from=` keywords).
///
/// # Safety
///
/// `object` must point to a live `PyBaseException`(-layout) instance.
pub(crate) unsafe fn set_exception_instance_attr(
	object: *mut PyObject,
	name_id: u32,
	value: *mut PyObject,
) {
	let exception = unsafe { &mut *object.cast::<PyBaseException>() };
	unsafe { (&mut *ensure_exception_dict(exception)).set(name_id, value) };
}

/// Synthesizes the `BaseException` method surface (`add_note`,
/// `with_traceback`) as bound natives; `None` when `name` is not one of them.
unsafe fn exception_synth_method(object: *mut PyObject, name: &str) -> Option<*mut PyObject> {
	type Entry = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;
	let entry: Entry = match name {
		"add_note" => exception_add_note,
		"with_traceback" => exception_with_traceback,
		_ => return None,
	};
	let function =
		unsafe { crate::abi::pon_make_function(entry as *const u8, 2, crate::intern::intern(name)) };
	if function.is_null() {
		return Some(ptr::null_mut());
	}
	match crate::types::method::new_bound_method(function, object) {
		Ok(method) => Some(method.cast::<PyObject>()),
		Err(message) => {
			crate::thread_state::pon_err_set(message);
			Some(ptr::null_mut())
		},
	}
}

/// `BaseException.add_note(note)` (PEP 678): append to `__notes__`, creating
/// the list in the instance dict on first use.
unsafe extern "C" fn exception_add_note(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argv.is_null() || argc != 2 {
		crate::thread_state::pon_err_set("add_note() takes exactly one argument");
		return ptr::null_mut();
	}
	let object = unsafe { *argv };
	let note = crate::tag::untag_arg(unsafe { *argv.add(1) });
	if unsafe { crate::types::type_::unicode_text(note) }.is_none() {
		let type_name = unsafe {
			let ty = if note.is_null() {
				ptr::null()
			} else {
				(*note).ob_type
			};
			if ty.is_null() { "object" } else { (*ty).name() }
		};
		let message = format!("note must be a str, not '{type_name}'");
		return unsafe { crate::abi::exc::pon_raise_type_error(message.as_ptr(), message.len()) };
	}
	let exception = unsafe { &mut *object.cast::<PyBaseException>() };
	let dict = unsafe { ensure_exception_dict(exception) };
	let notes_id = crate::intern::intern("__notes__");
	match unsafe { (&*dict).get(notes_id) } {
		Some(notes) => {
			if unsafe { crate::abi::seq::pon_list_append(notes, note) }.is_null() {
				return ptr::null_mut();
			}
		},
		None => {
			let notes = crate::native::builtins_mod::alloc_list(vec![note]);
			if notes.is_null() {
				return ptr::null_mut();
			}
			unsafe { (&mut *dict).set(notes_id, notes) };
		},
	}
	unsafe { crate::abi::pon_none() }
}

/// `BaseException.with_traceback(tb)`: install `tb` (or clear on None) and
/// return `self`.
unsafe extern "C" fn exception_with_traceback(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	if argv.is_null() || argc != 2 {
		crate::thread_state::pon_err_set("with_traceback() takes exactly one argument");
		return ptr::null_mut();
	}
	let object = unsafe { *argv };
	let tb = crate::tag::untag_arg(unsafe { *argv.add(1) });
	let exception = unsafe { &mut *object.cast::<PyBaseException>() };
	if tb.is_null() || tb == unsafe { crate::abi::pon_none() } {
		exception.traceback = ptr::null_mut();
		return object;
	}
	let is_traceback = unsafe { !(*tb).ob_type.is_null() && (*(*tb).ob_type).name() == "traceback" };
	if !is_traceback {
		const MESSAGE: &str = "__traceback__ must be a traceback or None";
		return unsafe { crate::abi::exc::pon_raise_type_error(MESSAGE.as_ptr(), MESSAGE.len()) };
	}
	exception.traceback = tb;
	object
}

/// `tp_setattro` for exception instances: fixed-surface names write the
/// C-level fields (mirroring CPython's `BaseException` getset setters); other
/// names live in the lazily-created instance dict.
pub(crate) unsafe extern "C" fn exception_setattro(
	object: *mut PyObject,
	name: *mut PyObject,
	value: *mut PyObject,
) -> c_int {
	let Some(name_text) = (unsafe { crate::types::type_::unicode_text(name) }) else {
		crate::thread_state::pon_err_set("exception attribute name must be str");
		return -1;
	};
	let name_id = crate::intern::intern(name_text);
	let obj_ty = unsafe { (*object).ob_type.cast_mut() };
	let descr = unsafe { crate::descr::lookup_in_type(obj_ty, name_id) };
	if unsafe { crate::descr::is_data_descriptor(descr) } {
		return unsafe { crate::descr::descriptor_set(descr, object, value) };
	}
	let none = unsafe { crate::abi::pon_none() };
	let exception = unsafe { &mut *object.cast::<PyBaseException>() };
	match name_text {
		"args" => {
			if value.is_null() {
				crate::thread_state::pon_err_set("args may not be deleted");
				return -1;
			}
			// CPython `BaseException.args` setter: any sequence, stored as a tuple.
			let mut argv = [value];
			let tuple = unsafe { crate::native::builtins_mod::builtin_tuple(argv.as_mut_ptr(), 1) };
			if tuple.is_null() {
				return -1;
			}
			exception.args = tuple;
			// Keep the legacy single-value slot coherent for diagnostics and
			// value-carrying readers (StopIteration.value, KeyError repr).
			let items = unsafe { (*tuple.cast::<crate::types::tuple::PyTuple>()).as_slice() };
			exception.message = items.first().copied().unwrap_or(ptr::null_mut());
			0
		},
		"__cause__" => {
			exception.cause = if value == none {
				ptr::null_mut()
			} else {
				value
			};
			if !value.is_null() {
				// CPython BaseException_set_cause: assignment implies
				// `__suppress_context__ = True`.
				exception.suppress_context = true;
			}
			0
		},
		"__context__" => {
			exception.context = if value == none {
				ptr::null_mut()
			} else {
				value
			};
			0
		},
		"__suppress_context__" => {
			exception.suppress_context =
				!value.is_null() && unsafe { crate::abi::pon_is_true(value) } == 1;
			0
		},
		"__traceback__" => {
			if value.is_null() || value == none {
				exception.traceback = ptr::null_mut();
				return 0;
			}
			let is_traceback =
				unsafe { !(*value).ob_type.is_null() && (*(*value).ob_type).name() == "traceback" };
			if !is_traceback {
				crate::thread_state::pon_err_set("__traceback__ must be a traceback or None");
				return -1;
			}
			exception.traceback = value;
			0
		},
		_ => {
			let dict = unsafe { ensure_exception_dict(exception) };
			if value.is_null() {
				if unsafe { (&mut *dict).del(name_id) } {
					0
				} else {
					unsafe { crate::abi::pon_raise_attribute_error(object, name_id) };
					-1
				}
			} else {
				unsafe { (&mut *dict).set(name_id, value) };
				0
			}
		},
	}
}

/// Applies `visit` to `ty` and every ancestor on its raw MRO, returning true
/// on the first hit.
///
/// CPython resolves `except` clauses with `PyType_IsSubtype`: a raw MRO walk
/// that never consults `__subclasscheck__` hooks.  Heap classes carry their C3
/// linearization in `tp_mro` (so a second base of a multiple-inheritance class
/// is visited); bootstrap builtin exception types have no carrier and fall
/// back to the single-inheritance `tp_base` chain.
///
/// # Safety
///
/// `ty` must be NULL or point to a live `PyType` object.
unsafe fn walk_raw_mro(ty: *const PyType, mut visit: impl FnMut(*const PyType) -> bool) -> bool {
	if ty.is_null() {
		return false;
	}
	// SAFETY: `ty` is a live type object per the caller contract.
	let carrier = unsafe { (*ty).tp_mro };
	if !carrier.is_null() {
		// SAFETY: A non-NULL `tp_mro` is always a live boxed `PyMro` carrier.
		let mro = unsafe { &*carrier.cast::<crate::mro::PyMro>() };
		return mro
			.entries()
			.iter()
			.any(|&entry| !entry.is_null() && visit(entry.cast_const()));
	}
	let mut current = ty;
	while !current.is_null() {
		if visit(current) {
			return true;
		}
		// SAFETY: `current` walks live type objects; `tp_base` is live or NULL.
		current = unsafe { (*current).tp_base.cast_const() };
	}
	false
}

/// Returns true when `ty` or any ancestor on its raw MRO is named `name`.
///
/// # Safety
///
/// `ty` must be NULL or point to a live `PyType` object.
pub unsafe fn exception_type_named(ty: *const PyType, name: &str) -> bool {
	// SAFETY: Entries visited by `walk_raw_mro` are live non-NULL type objects.
	unsafe { walk_raw_mro(ty, |entry| (*entry).name() == name) }
}

/// Returns true when `sub` is `base` or inherits from it through the raw MRO.
///
/// This is the except-clause matching predicate: CPython uses
/// `PyType_IsSubtype` here, so metaclass `__subclasscheck__` hooks must never
/// fire on this path.
///
/// # Safety
///
/// Non-NULL pointers must point to live `PyType` objects.
pub unsafe fn is_exception_subclass(sub: *const PyType, base: *const PyType) -> bool {
	if sub.is_null() || base.is_null() {
		return false;
	}
	if sub == base {
		return true;
	}
	// Builtin `ExceptionGroup` derives from both `BaseExceptionGroup` and
	// `Exception` in CPython; pon's bootstrap descriptor records only the
	// group chain, so the `Exception` edge is restored by name.
	// SAFETY: `base` is live per the caller contract; entries visited by
	// `walk_raw_mro` are live non-NULL type objects.
	unsafe {
		let wants_exception = (*base).name() == "Exception";
		walk_raw_mro(sub, |entry| {
			entry == base || (wants_exception && (*entry).name() == "ExceptionGroup")
		})
	}
}

/// Returns true when `object` is a boxed exception instance matching `ty`.
///
/// # Safety
///
/// Non-NULL pointers must point to live boxed objects/type descriptors.
pub unsafe fn is_exception_instance(object: *mut PyObject, ty: *const PyType) -> bool {
	if object.is_null() {
		return false;
	}
	// SAFETY: Caller guarantees `object` is a live boxed object.
	let object_type = unsafe { (*object).ob_type };
	// SAFETY: Caller guarantees the object's type is a live type descriptor.
	unsafe { is_exception_subclass(object_type, ty) }
}

/// Casts a base-exception instance to the ABI object pointer.
#[must_use]
pub fn as_exception_object(exception: *mut PyBaseException) -> *mut PyObject {
	as_object_ptr(exception)
}

/// Traces the boxed pointers stored in a `PyBaseException`.
///
/// # Safety
///
/// `object` must be NULL or point to a live `PyBaseException` allocation.
pub unsafe extern "C" fn trace_base_exception(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
	if object.is_null() {
		return;
	}

	// SAFETY: The GC registered this callback only for `PyBaseException`
	// allocations.
	let exception = unsafe { &*object.cast::<PyBaseException>() };
	for child in
		[exception.message, exception.cause, exception.context, exception.traceback, exception.args]
	{
		if !child.is_null() {
			visitor(child.cast::<u8>());
		}
	}
	if !exception.dict.is_null() {
		for (_, value) in unsafe { (&*exception.dict).iter() } {
			if !value.is_null() {
				visitor(value.cast::<u8>());
			}
		}
	}
}

/// Traces the boxed pointers stored in a `PyExceptionGroup`.
///
/// # Safety
///
/// `object` must be NULL or point to a live `PyExceptionGroup` allocation.
pub unsafe extern "C" fn trace_exception_group(object: *mut u8, visitor: &mut dyn FnMut(*mut u8)) {
	if object.is_null() {
		return;
	}
	unsafe { trace_base_exception(object, visitor) };
	let group = unsafe { &*object.cast::<PyExceptionGroup>() };
	if !group.exceptions.is_null() {
		visitor(group.exceptions.cast::<u8>());
	}
}

/// Releases the Rust-owned instance dictionary of a boxed exception (shared
/// by plain exceptions and groups, which embed the same prefix).
///
/// # Safety
///
/// `object` must be NULL or point to a dead `PyBaseException`(-prefixed)
/// allocation owned by the GC.
pub unsafe extern "C" fn finalize_base_exception(object: *mut u8) {
	if object.is_null() {
		return;
	}
	let exception = unsafe { &mut *object.cast::<PyBaseException>() };
	if !exception.dict.is_null() {
		unsafe { drop(Box::from_raw(exception.dict)) };
		exception.dict = ptr::null_mut();
	}
}

const _: () = {
	assert!(offset_of!(PyBaseException, ob_base) == 0);
	assert!(offset_of!(PyExceptionGroup, base) == 0);
	assert!(size_of::<PyObject>() == size_of::<PyObjectHeader>());
};

#[cfg(test)]
mod tests {
	use core::ptr;

	use super::{exception_type_named, is_exception_subclass};
	use crate::{
		mro::set_c3_mro,
		object::PyType,
		thread_state::{pon_err_clear, test_state_lock},
	};

	#[test]
	fn second_base_matches_through_mro_carrier() {
		let _guard = test_state_lock();
		pon_err_clear();
		let mut type_type = PyType::new(ptr::null(), "type", core::mem::size_of::<PyType>());
		let type_ptr = &mut type_type as *mut PyType;
		unsafe { (*type_ptr).ob_base.ob_type = type_ptr };

		let mut first = PyType::new(type_ptr, "FirstBase", 0);
		let first_ptr = &mut first as *mut PyType;
		let mut second = PyType::new(type_ptr, "SecondBase", 0);
		let second_ptr = &mut second as *mut PyType;
		let mut unrelated = PyType::new(type_ptr, "Unrelated", 0);
		let unrelated_ptr = &mut unrelated as *mut PyType;
		let mut sub = PyType::new(type_ptr, "MultiSub", 0);
		sub.tp_base = first_ptr;
		let sub_ptr = &mut sub as *mut PyType;

		unsafe {
			assert_eq!(set_c3_mro(sub_ptr, &[first_ptr, second_ptr]), 0);
			// The regression: SecondBase is unreachable via tp_base (sub.tp_base
			// is FirstBase only) and must be found through the C3 carrier.
			assert!(is_exception_subclass(sub_ptr, second_ptr));
			assert!(is_exception_subclass(sub_ptr, first_ptr));
			assert!(!is_exception_subclass(sub_ptr, unrelated_ptr));
		}
	}

	#[test]
	fn tp_base_chain_fallback_without_carrier() {
		let mut root = PyType::new(ptr::null(), "RootErr", 0);
		let root_ptr = &mut root as *mut PyType;
		let mut mid = PyType::new(ptr::null(), "MidErr", 0);
		mid.tp_base = root_ptr;
		let mid_ptr = &mut mid as *mut PyType;
		let mut leaf = PyType::new(ptr::null(), "LeafErr", 0);
		leaf.tp_base = mid_ptr;
		let leaf_ptr = &mut leaf as *mut PyType;
		let mut stranger = PyType::new(ptr::null(), "Stranger", 0);
		let stranger_ptr = &mut stranger as *mut PyType;

		unsafe {
			assert!(is_exception_subclass(leaf_ptr, leaf_ptr));
			assert!(is_exception_subclass(leaf_ptr, mid_ptr));
			assert!(is_exception_subclass(leaf_ptr, root_ptr));
			assert!(!is_exception_subclass(leaf_ptr, stranger_ptr));
			// Direction matters: an ancestor never matches its descendant.
			assert!(!is_exception_subclass(root_ptr, leaf_ptr));
			assert!(!is_exception_subclass(ptr::null(), root_ptr));
			assert!(!is_exception_subclass(leaf_ptr, ptr::null()));
		}
	}

	#[test]
	fn exception_group_matches_exception_by_name() {
		let mut base_group = PyType::new(ptr::null(), "BaseExceptionGroup", 0);
		let base_group_ptr = &mut base_group as *mut PyType;
		let mut group = PyType::new(ptr::null(), "ExceptionGroup", 0);
		group.tp_base = base_group_ptr;
		let group_ptr = &mut group as *mut PyType;
		let mut exception = PyType::new(ptr::null(), "Exception", 0);
		let exception_ptr = &mut exception as *mut PyType;
		let mut type_error = PyType::new(ptr::null(), "TypeError", 0);
		let type_error_ptr = &mut type_error as *mut PyType;

		unsafe {
			// "Exception" is nowhere on the group's tp_base chain; the edge is
			// restored by name for the carrier-less builtin descriptor.
			assert!(is_exception_subclass(group_ptr, exception_ptr));
			// The name gate is base-side only: the same walk must not match a
			// base that is not named "Exception".
			assert!(!is_exception_subclass(group_ptr, type_error_ptr));
			assert!(!is_exception_subclass(exception_ptr, group_ptr));
		}
	}

	#[test]
	fn carried_exception_group_entry_matches_exception() {
		let _guard = test_state_lock();
		pon_err_clear();
		let mut type_type = PyType::new(ptr::null(), "type", core::mem::size_of::<PyType>());
		let type_ptr = &mut type_type as *mut PyType;
		unsafe { (*type_ptr).ob_base.ob_type = type_ptr };

		let mut group = PyType::new(type_ptr, "ExceptionGroup", 0);
		let group_ptr = &mut group as *mut PyType;
		// tp_base deliberately left NULL: the "ExceptionGroup" entry is
		// reachable ONLY through the carrier, never the tp_base chain.
		let mut derived = PyType::new(type_ptr, "DerivedGroup", 0);
		let derived_ptr = &mut derived as *mut PyType;
		let mut exception = PyType::new(type_ptr, "Exception", 0);
		let exception_ptr = &mut exception as *mut PyType;

		unsafe {
			assert_eq!(set_c3_mro(derived_ptr, &[group_ptr]), 0);
			// "Exception" is absent from the carrier; the entry NAMED
			// "ExceptionGroup" (not the head type, which is "DerivedGroup")
			// must restore the edge on the carrier path too.
			assert!(is_exception_subclass(derived_ptr, exception_ptr));
		}
	}

	#[test]
	fn exception_type_named_walks_carrier() {
		let _guard = test_state_lock();
		pon_err_clear();
		let mut type_type = PyType::new(ptr::null(), "type", core::mem::size_of::<PyType>());
		let type_ptr = &mut type_type as *mut PyType;
		unsafe { (*type_ptr).ob_base.ob_type = type_ptr };

		let mut first = PyType::new(type_ptr, "FirstBase", 0);
		let first_ptr = &mut first as *mut PyType;
		let mut second = PyType::new(type_ptr, "SecondBase", 0);
		let second_ptr = &mut second as *mut PyType;
		let mut sub = PyType::new(type_ptr, "MultiSub", 0);
		sub.tp_base = first_ptr;
		let sub_ptr = &mut sub as *mut PyType;

		unsafe {
			assert_eq!(set_c3_mro(sub_ptr, &[first_ptr, second_ptr]), 0);
			// Second-base NAME must be visible through the carrier.
			assert!(exception_type_named(sub_ptr, "SecondBase"));
			assert!(exception_type_named(sub_ptr, "FirstBase"));
			assert!(exception_type_named(sub_ptr, "MultiSub"));
			assert!(!exception_type_named(sub_ptr, "Absent"));
		}
	}

	#[test]
	fn exception_type_named_walks_tp_base_chain() {
		let mut root = PyType::new(ptr::null(), "RootErr", 0);
		let root_ptr = &mut root as *mut PyType;
		let mut leaf = PyType::new(ptr::null(), "LeafErr", 0);
		leaf.tp_base = root_ptr;
		let leaf_ptr = &mut leaf as *mut PyType;

		unsafe {
			assert!(exception_type_named(leaf_ptr, "RootErr"));
			assert!(exception_type_named(leaf_ptr, "LeafErr"));
			assert!(!exception_type_named(leaf_ptr, "Absent"));
			assert!(!exception_type_named(ptr::null(), "RootErr"));
		}
	}
}
