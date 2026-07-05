//! Native `_sha2` module: SHA-2 family constructors for hashlib/random.
//!
//! `random.py` imports `sha512` at module top (`from _sha2 import sha512`) and
//! `hashlib.py` expects `_sha2` to expose `sha224`, `sha256`, `sha384`, and
//! `sha512` during module init. The hash objects buffer their input and compute
//! on demand; the surface is the digest-object subset the stdlib chain consumes
//! (`update`/`digest`/`hexdigest`/`copy` plus the standard metadata attrs).

use std::{ptr, sync::LazyLock};

use ::sha2::Digest as _;

use super::{builtins_mod::VARIADIC_ARITY, install_module};
use crate::{
	abi,
	intern::intern,
	object::{PyObject, PyObjectHeader, PyType},
	types::{bytearray_ as bytearray_type, bytes_ as bytes_type, exc::ExceptionKind},
};

const HASHLIB_GIL_MINSIZE: i64 = 2048;

fn sha224_digest(message: &[u8]) -> [u8; 28] {
	::sha2::Sha224::digest(message).into()
}

fn sha256_digest(message: &[u8]) -> [u8; 32] {
	::sha2::Sha256::digest(message).into()
}

fn sha384_digest(message: &[u8]) -> [u8; 48] {
	::sha2::Sha384::digest(message).into()
}

/// FIPS 180-4 SHA-512 over the full message (single-shot; callers buffer).
fn sha512_digest(message: &[u8]) -> [u8; 64] {
	::sha2::Sha512::digest(message).into()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Sha2Kind {
	Sha224,
	Sha256,
	Sha384,
	Sha512,
}

impl Sha2Kind {
	const ALL: [Self; 4] = [Self::Sha224, Self::Sha256, Self::Sha384, Self::Sha512];

	const fn constructor_name(self) -> &'static str {
		match self {
			Self::Sha224 => "sha224",
			Self::Sha256 => "sha256",
			Self::Sha384 => "sha384",
			Self::Sha512 => "sha512",
		}
	}

	const fn type_name(self) -> &'static str {
		match self {
			Self::Sha224 => "SHA224Type",
			Self::Sha256 => "SHA256Type",
			Self::Sha384 => "SHA384Type",
			Self::Sha512 => "SHA512Type",
		}
	}

	const fn digest_size(self) -> i64 {
		match self {
			Self::Sha224 => 28,
			Self::Sha256 => 32,
			Self::Sha384 => 48,
			Self::Sha512 => 64,
		}
	}

	const fn block_size(self) -> i64 {
		match self {
			Self::Sha224 | Self::Sha256 => 64,
			Self::Sha384 | Self::Sha512 => 128,
		}
	}
}

fn sha2_type_slot(name: &'static str) -> usize {
	let mut ty =
		PyType::new(abi::runtime_type_type().cast_const(), name, core::mem::size_of::<PySha2>());
	ty.tp_getattro = Some(sha2_getattro);
	Box::into_raw(Box::new(ty)) as usize
}

fn sha2_type(kind: Sha2Kind) -> *mut PyType {
	match kind {
		Sha2Kind::Sha224 => *SHA224_TYPE as *mut PyType,
		Sha2Kind::Sha256 => *SHA256_TYPE as *mut PyType,
		Sha2Kind::Sha384 => *SHA384_TYPE as *mut PyType,
		Sha2Kind::Sha512 => *SHA512_TYPE as *mut PyType,
	}
}

fn is_sha2_type(ty: *const PyType) -> bool {
	ty == sha2_type(Sha2Kind::Sha224).cast_const()
		|| ty == sha2_type(Sha2Kind::Sha256).cast_const()
		|| ty == sha2_type(Sha2Kind::Sha384).cast_const()
		|| ty == sha2_type(Sha2Kind::Sha512).cast_const()
}

static SHA224_TYPE: LazyLock<usize> =
	LazyLock::new(|| sha2_type_slot(Sha2Kind::Sha224.constructor_name()));
static SHA256_TYPE: LazyLock<usize> =
	LazyLock::new(|| sha2_type_slot(Sha2Kind::Sha256.constructor_name()));
static SHA384_TYPE: LazyLock<usize> =
	LazyLock::new(|| sha2_type_slot(Sha2Kind::Sha384.constructor_name()));
static SHA512_TYPE: LazyLock<usize> =
	LazyLock::new(|| sha2_type_slot(Sha2Kind::Sha512.constructor_name()));

// ---------------------------------------------------------------------------
// Hash object

#[repr(C)]
struct PySha2 {
	ob_base: PyObjectHeader,
	kind:    Sha2Kind,
	/// Buffered message; the digest is computed on demand.
	data:    Vec<u8>,
}

fn untag(object: *mut PyObject) -> *mut PyObject {
	crate::tag::untag_arg(object)
}

fn raise(kind: ExceptionKind, message: &str) -> *mut PyObject {
	crate::abi::exc::raise_kind_error_text(kind, message)
}

/// Borrows a bytes/bytearray payload without raising; `None` for other types.
fn bytes_like<'a>(object: *mut PyObject) -> Option<&'a [u8]> {
	if object.is_null() {
		return None;
	}
	// SAFETY: A non-NULL heap object carries a live header.
	let ty = unsafe { (*object).ob_type };
	if bytes_type::is_bytes_type(ty) {
		// SAFETY: Type check above proved the layout.
		return Some(unsafe { (*object.cast::<bytes_type::PyBytes>()).as_slice() });
	}
	if bytearray_type::is_bytearray_type(ty) {
		// SAFETY: Type check above proved the layout.
		return Some(unsafe { (*object.cast::<bytearray_type::PyByteArray>()).as_slice() });
	}
	None
}

fn alloc_sha2(kind: Sha2Kind, data: Vec<u8>) -> *mut PyObject {
	let object = Box::new(PySha2 { ob_base: PyObjectHeader::new(sha2_type(kind)), kind, data });
	Box::into_raw(object).cast::<PyObject>()
}

unsafe fn receiver<'a>(object: *mut PyObject) -> Option<&'a mut PySha2> {
	let object = untag(object);
	if object.is_null() {
		return None;
	}
	// SAFETY: A non-NULL heap object carries a live header.
	if !is_sha2_type(unsafe { (*object).ob_type }) {
		return None;
	}
	// SAFETY: Type check above proved the layout.
	Some(unsafe { &mut *object.cast::<PySha2>() })
}

unsafe fn receiver_kind(object: *mut PyObject) -> Option<Sha2Kind> {
	unsafe { receiver(object) }.map(|this| this.kind)
}

fn constructor_error(kind: Sha2Kind, argc: usize) -> *mut PyObject {
	raise(
		ExceptionKind::TypeError,
		&format!("{}() takes at most 2 arguments ({argc} given)", kind.constructor_name()),
	)
}

fn sha2_new(kind: Sha2Kind, argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	if argc > 2 {
		return constructor_error(kind, argc);
	}
	let mut data = Vec::new();
	if argc >= 1 && !argv.is_null() {
		// SAFETY: One live argument slot per the check above.
		let arg = untag(unsafe { *argv });
		// The optional trailing argument is `usedforsecurity`; ignore it.
		// SAFETY: Type probe tolerates any live object.
		if unsafe { crate::types::dict::type_name(arg) } != Some("NoneType") {
			let Some(payload) = bytes_like(arg) else {
				return raise(
					ExceptionKind::TypeError,
					&format!(
						"object supporting the buffer API required, not '{}'",
						// SAFETY: Same contract as above.
						unsafe { crate::types::dict::type_name(arg) }.unwrap_or("object")
					),
				);
			};
			data = payload.to_vec();
		}
	}
	alloc_sha2(kind, data)
}

macro_rules! sha_constructor {
	($entry:ident, $kind:expr) => {
		unsafe extern "C" fn $entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
			sha2_new($kind, argv, argc)
		}
	};
}

sha_constructor!(sha224_new, Sha2Kind::Sha224);
sha_constructor!(sha256_new, Sha2Kind::Sha256);
sha_constructor!(sha384_new, Sha2Kind::Sha384);
sha_constructor!(sha512_new, Sha2Kind::Sha512);

macro_rules! sha_method {
	($entry:ident, $name:literal, $body:expr) => {
		unsafe extern "C" fn $entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
			if argv.is_null() || argc == 0 {
				return raise(ExceptionKind::TypeError, concat!($name, "() missing receiver"));
			}
			// SAFETY: The caller passes a live argv window of length argc.
			let raw = unsafe { core::slice::from_raw_parts(argv, argc) };
			// SAFETY: Bound receiver from this type's getattro.
			let Some(this) = (unsafe { receiver(raw[0]) }) else {
				return raise(
					ExceptionKind::TypeError,
					concat!($name, "() receiver must be a sha2 object"),
				);
			};
			let args: Vec<*mut PyObject> = raw[1..].iter().copied().map(untag).collect();
			#[allow(clippy::redundant_closure_call)]
			($body)(this, &args)
		}
	};
}

fn pon_bytes_from_digest(digest: &[u8]) -> *mut PyObject {
	// SAFETY: Runtime allocation helper; NULL on failure with the error set.
	unsafe { abi::str_::pon_const_bytes(digest.as_ptr(), digest.len()) }
}

fn hex_digest(digest: &[u8]) -> String {
	let mut hex = String::with_capacity(digest.len() * 2);
	for byte in digest {
		hex.push_str(&format!("{byte:02x}"));
	}
	hex
}

sha_method!(sha2_update, "update", |this: &mut PySha2, args: &[*mut PyObject]| {
	if args.len() != 1 {
		return raise(ExceptionKind::TypeError, "update() takes exactly 1 argument");
	}
	let Some(payload) = bytes_like(args[0]) else {
		return raise(ExceptionKind::TypeError, "object supporting the buffer API required");
	};
	this.data.extend_from_slice(payload);
	// SAFETY: Singleton accessor.
	unsafe { abi::pon_none() }
});

sha_method!(sha2_digest_method, "digest", |this: &mut PySha2, args: &[*mut PyObject]| {
	if !args.is_empty() {
		return raise(ExceptionKind::TypeError, "digest() takes no arguments");
	}
	match this.kind {
		Sha2Kind::Sha224 => pon_bytes_from_digest(&sha224_digest(&this.data)),
		Sha2Kind::Sha256 => pon_bytes_from_digest(&sha256_digest(&this.data)),
		Sha2Kind::Sha384 => pon_bytes_from_digest(&sha384_digest(&this.data)),
		Sha2Kind::Sha512 => pon_bytes_from_digest(&sha512_digest(&this.data)),
	}
});

sha_method!(sha2_hexdigest, "hexdigest", |this: &mut PySha2, args: &[*mut PyObject]| {
	if !args.is_empty() {
		return raise(ExceptionKind::TypeError, "hexdigest() takes no arguments");
	}
	let hex = match this.kind {
		Sha2Kind::Sha224 => hex_digest(&sha224_digest(&this.data)),
		Sha2Kind::Sha256 => hex_digest(&sha256_digest(&this.data)),
		Sha2Kind::Sha384 => hex_digest(&sha384_digest(&this.data)),
		Sha2Kind::Sha512 => hex_digest(&sha512_digest(&this.data)),
	};
	// SAFETY: Runtime allocation helper; NULL on failure with the error set.
	unsafe { abi::pon_const_str(hex.as_ptr(), hex.len()) }
});

sha_method!(sha2_copy, "copy", |this: &mut PySha2, args: &[*mut PyObject]| {
	if !args.is_empty() {
		return raise(ExceptionKind::TypeError, "copy() takes no arguments");
	}
	alloc_sha2(this.kind, this.data.clone())
});

unsafe extern "C" fn sha2_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let name = untag(name);
	// SAFETY: `unicode_text` type-checks its argument.
	let Some(name_text) = (unsafe { crate::types::type_::unicode_text(name) }) else {
		crate::thread_state::pon_err_set("attribute name must be str");
		return ptr::null_mut();
	};
	let Some(kind) = (unsafe { receiver_kind(object) }) else {
		return raise(ExceptionKind::TypeError, "sha2 getattro on non-sha2 receiver");
	};
	let entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject = match name_text {
		// SAFETY: Runtime allocation helpers follow the NULL-sentinel contract.
		"name" => {
			let constructor_name = kind.constructor_name();
			return unsafe { abi::pon_const_str(constructor_name.as_ptr(), constructor_name.len()) };
		},
		// SAFETY: Same contract as above.
		"digest_size" => return unsafe { abi::pon_const_int(kind.digest_size()) },
		// SAFETY: Same contract as above.
		"block_size" => return unsafe { abi::pon_const_int(kind.block_size()) },
		"update" => sha2_update,
		"digest" => sha2_digest_method,
		"hexdigest" => sha2_hexdigest,
		"copy" => sha2_copy,
		// SAFETY: Typed raise helper.
		_ => return unsafe { abi::exc::pon_raise_attribute_error(object, intern(name_text)) },
	};
	let interned = intern(name_text);
	// SAFETY: Live builtin entry point with the runtime calling convention.
	let function = unsafe { abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, interned) };
	if function.is_null() {
		return ptr::null_mut();
	}
	match crate::types::method::new_bound_method(function, object) {
		Ok(method) => method.cast::<PyObject>(),
		Err(message) => raise(ExceptionKind::TypeError, &message),
	}
}

// ---------------------------------------------------------------------------
// Module factory

fn constructor_entry(
	kind: Sha2Kind,
) -> unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject {
	match kind {
		Sha2Kind::Sha224 => sha224_new,
		Sha2Kind::Sha256 => sha256_new,
		Sha2Kind::Sha384 => sha384_new,
		Sha2Kind::Sha512 => sha512_new,
	}
}

fn int_attr(name: &str, value: i64) -> Result<(u32, *mut PyObject), String> {
	let object = unsafe { abi::pon_const_int(value) };
	if object.is_null() {
		return Err(format!("failed to allocate _sha2.{name}"));
	}
	Ok((intern(name), object))
}

fn module_constructor(kind: Sha2Kind) -> Result<*mut PyObject, String> {
	let name = kind.constructor_name();
	// SAFETY: Live builtin entry point with the runtime calling convention.
	let constructor = unsafe {
		abi::pon_make_function(constructor_entry(kind) as *const u8, VARIADIC_ARITY, intern(name))
	};
	if constructor.is_null() {
		return Err(format!("failed to allocate _sha2.{name}"));
	}
	Ok(constructor)
}

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	let name = "_sha2";
	// SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
	let name_object = unsafe { abi::pon_const_str(name.as_ptr(), name.len()) };
	if name_object.is_null() {
		return Err("failed to allocate _sha2.__name__".to_owned());
	}
	let mut attrs =
		vec![(intern("__name__"), name_object), int_attr("_GIL_MINSIZE", HASHLIB_GIL_MINSIZE)?];
	for kind in Sha2Kind::ALL {
		attrs.push((intern(kind.constructor_name()), module_constructor(kind)?));
		attrs.push((intern(kind.type_name()), sha2_type(kind).cast::<PyObject>()));
	}
	install_module(name, attrs)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn hex64(digest: [u8; 64]) -> String {
		digest.iter().map(|byte| format!("{byte:02x}")).collect()
	}

	#[test]
	fn empty_message_matches_fips_vector() {
		assert_eq!(
            hex64(sha512_digest(b"")),
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce\
             47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
        );
	}

	#[test]
	fn abc_matches_fips_vector() {
		assert_eq!(
            hex64(sha512_digest(b"abc")),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
             2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
	}

	#[test]
	fn two_block_896_bit_message_matches_fips_vector() {
		// FIPS 180-4 two-block SHA-512 vector (112-byte message).
		assert_eq!(
            hex64(sha512_digest(
                b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmno\
                  ijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu"
            )),
            "8e959b75dae313da8cf4f72814fc143f8f7779c6eb9f7fa17299aeadb6889018\
             501d289e4900f7e4331b99dec4b5433ac7d329eeb6dd26545e96e55b874be909"
        );
	}

	#[test]
	fn padding_length_boundaries_match_cpython() {
		// Expected digests computed with python3 hashlib during authoring.
		// 111 bytes: 0x80 lands flush against the 16-byte length slot (one
		//            block, zero fill).
		// 112 bytes: one byte past that edge, forcing a padding-only block.
		// 128 bytes: exactly one full data block plus a padding-only block.
		// 200 bytes: multi-block with a partial trailing block.
		for (len, expected) in [
            (
                111,
                "fa9121c7b32b9e01733d034cfc78cbf67f926c7ed83e82200ef86818196921760b4beff48404df811b953828274461673c68d04e297b0eb7b2b4d60fc6b566a2",
            ),
            (
                112,
                "c01d080efd492776a1c43bd23dd99d0a2e626d481e16782e75d54c2503b5dc32bd05f0f1ba33e568b88fd2d970929b719ecbb152f58f130a407c8830604b70ca",
            ),
            (
                128,
                "b73d1929aa615934e61a871596b3f3b33359f42b8175602e89f7e06e5f658a243667807ed300314b95cacdd579f3e33abdfbe351909519a846d465c59582f321",
            ),
            (
                200,
                "4b11459c33f52a22ee8236782714c150a3b2c60994e9acee17fe68947a3e6789f31e7668394592da7bef827cddca88c4e6f86e4df7ed1ae6cba71f3e98faee9f",
            ),
        ] {
            assert_eq!(hex64(sha512_digest(&vec![0x61u8; len])), expected, "'a' * {len}");
        }
	}
}
