//! Native hashlib fallback modules for MD5, SHA-1, SHA-3/SHAKE, and BLAKE2.
//!
//! Vendored `hashlib.py` imports these CPython extension module names when
//! `_hashlib` is unavailable.  The objects mirror the existing `_sha2` surface:
//! mutable buffered hash objects with `update`, `digest`, `hexdigest`, `copy`,
//! `name`, `digest_size`, and `block_size`.

use std::{ptr, sync::LazyLock};

use ::blake2::digest::{
	Output, Update as _,
	core_api::{CoreWrapper, VariableOutputCore},
};
use ::md5::Digest as _;
use ::sha3::digest::{ExtendableOutput as _, XofReader as _};
use num_traits::ToPrimitive as _;

use super::{builtins_mod::VARIADIC_ARITY, install_module};
use crate::{
	abi,
	intern::intern,
	object::{PyObject, PyObjectHeader, PyType},
	types::{bytearray_ as bytearray_type, bytes_ as bytes_type, exc::ExceptionKind},
};

const BLAKE2_ARG_SLOTS: usize = 13;
const BLAKE2B_MAX_DIGEST_SIZE: usize = 64;
const BLAKE2S_MAX_DIGEST_SIZE: usize = 32;
const BLAKE2B_MAX_KEY_SIZE: usize = 64;
const BLAKE2S_MAX_KEY_SIZE: usize = 32;
const BLAKE2B_BLOCK_SIZE: usize = 128;
const BLAKE2S_BLOCK_SIZE: usize = 64;
const BLAKE2B_PERSON_SIZE: usize = 16;
const BLAKE2S_PERSON_SIZE: usize = 8;
const HASHLIB_GIL_MINSIZE: i64 = 2048;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HashKind {
	Md5,
	Sha1,
	Sha3_224,
	Sha3_256,
	Sha3_384,
	Sha3_512,
	Shake128,
	Shake256,
	Blake2b,
	Blake2s,
}

impl HashKind {
	const BLAKE2: [Self; 2] = [Self::Blake2b, Self::Blake2s];
	const MD5: [Self; 1] = [Self::Md5];
	const SHA1: [Self; 1] = [Self::Sha1];
	const SHA3: [Self; 6] = [
		Self::Sha3_224,
		Self::Sha3_256,
		Self::Sha3_384,
		Self::Sha3_512,
		Self::Shake128,
		Self::Shake256,
	];

	const fn constructor_name(self) -> &'static str {
		match self {
			Self::Md5 => "md5",
			Self::Sha1 => "sha1",
			Self::Sha3_224 => "sha3_224",
			Self::Sha3_256 => "sha3_256",
			Self::Sha3_384 => "sha3_384",
			Self::Sha3_512 => "sha3_512",
			Self::Shake128 => "shake_128",
			Self::Shake256 => "shake_256",
			Self::Blake2b => "blake2b",
			Self::Blake2s => "blake2s",
		}
	}

	const fn default_digest_size(self) -> usize {
		match self {
			Self::Md5 => 16,
			Self::Sha1 => 20,
			Self::Sha3_224 => 28,
			Self::Sha3_256 => 32,
			Self::Sha3_384 => 48,
			Self::Sha3_512 => 64,
			Self::Shake128 | Self::Shake256 => 0,
			Self::Blake2b => BLAKE2B_MAX_DIGEST_SIZE,
			Self::Blake2s => BLAKE2S_MAX_DIGEST_SIZE,
		}
	}

	const fn block_size(self) -> i64 {
		match self {
			Self::Md5 | Self::Sha1 | Self::Blake2s => 64,
			Self::Sha3_224 => 144,
			Self::Sha3_256 | Self::Shake256 => 136,
			Self::Sha3_384 => 104,
			Self::Sha3_512 => 72,
			Self::Shake128 => 168,
			Self::Blake2b => 128,
		}
	}

	const fn is_blake2(self) -> bool {
		matches!(self, Self::Blake2b | Self::Blake2s)
	}

	const fn is_shake(self) -> bool {
		matches!(self, Self::Shake128 | Self::Shake256)
	}
}

#[derive(Clone)]
struct Blake2Params {
	digest_size: usize,
	key:         Vec<u8>,
	salt:        Vec<u8>,
	person:      Vec<u8>,
}

impl Blake2Params {
	fn default_for(kind: HashKind) -> Self {
		Self {
			digest_size: kind.default_digest_size(),
			key:         Vec::new(),
			salt:        Vec::new(),
			person:      Vec::new(),
		}
	}
}

#[repr(C)]
struct PyHash {
	ob_base: PyObjectHeader,
	kind:    HashKind,
	/// Buffered message; the digest is computed on demand.
	data:    Vec<u8>,
	blake2:  Option<Blake2Params>,
}

impl PyHash {
	fn digest_size(&self) -> usize {
		self
			.blake2
			.as_ref()
			.map_or_else(|| self.kind.default_digest_size(), |params| params.digest_size)
	}
}

fn hash_type_slot(name: &'static str) -> usize {
	let mut ty =
		PyType::new(abi::runtime_type_type().cast_const(), name, core::mem::size_of::<PyHash>());
	ty.tp_getattro = Some(hash_getattro);
	Box::into_raw(Box::new(ty)) as usize
}

fn hash_type(kind: HashKind) -> *mut PyType {
	match kind {
		HashKind::Md5 => *MD5_TYPE as *mut PyType,
		HashKind::Sha1 => *SHA1_TYPE as *mut PyType,
		HashKind::Sha3_224 => *SHA3_224_TYPE as *mut PyType,
		HashKind::Sha3_256 => *SHA3_256_TYPE as *mut PyType,
		HashKind::Sha3_384 => *SHA3_384_TYPE as *mut PyType,
		HashKind::Sha3_512 => *SHA3_512_TYPE as *mut PyType,
		HashKind::Shake128 => *SHAKE_128_TYPE as *mut PyType,
		HashKind::Shake256 => *SHAKE_256_TYPE as *mut PyType,
		HashKind::Blake2b => *BLAKE2B_TYPE as *mut PyType,
		HashKind::Blake2s => *BLAKE2S_TYPE as *mut PyType,
	}
}

fn is_hash_type(ty: *const PyType) -> bool {
	ty == hash_type(HashKind::Md5).cast_const()
		|| ty == hash_type(HashKind::Sha1).cast_const()
		|| ty == hash_type(HashKind::Sha3_224).cast_const()
		|| ty == hash_type(HashKind::Sha3_256).cast_const()
		|| ty == hash_type(HashKind::Sha3_384).cast_const()
		|| ty == hash_type(HashKind::Sha3_512).cast_const()
		|| ty == hash_type(HashKind::Shake128).cast_const()
		|| ty == hash_type(HashKind::Shake256).cast_const()
		|| ty == hash_type(HashKind::Blake2b).cast_const()
		|| ty == hash_type(HashKind::Blake2s).cast_const()
}

static MD5_TYPE: LazyLock<usize> =
	LazyLock::new(|| hash_type_slot(HashKind::Md5.constructor_name()));
static SHA1_TYPE: LazyLock<usize> =
	LazyLock::new(|| hash_type_slot(HashKind::Sha1.constructor_name()));
static SHA3_224_TYPE: LazyLock<usize> =
	LazyLock::new(|| hash_type_slot(HashKind::Sha3_224.constructor_name()));
static SHA3_256_TYPE: LazyLock<usize> =
	LazyLock::new(|| hash_type_slot(HashKind::Sha3_256.constructor_name()));
static SHA3_384_TYPE: LazyLock<usize> =
	LazyLock::new(|| hash_type_slot(HashKind::Sha3_384.constructor_name()));
static SHA3_512_TYPE: LazyLock<usize> =
	LazyLock::new(|| hash_type_slot(HashKind::Sha3_512.constructor_name()));
static SHAKE_128_TYPE: LazyLock<usize> =
	LazyLock::new(|| hash_type_slot(HashKind::Shake128.constructor_name()));
static SHAKE_256_TYPE: LazyLock<usize> =
	LazyLock::new(|| hash_type_slot(HashKind::Shake256.constructor_name()));
static BLAKE2B_TYPE: LazyLock<usize> =
	LazyLock::new(|| hash_type_slot(HashKind::Blake2b.constructor_name()));
static BLAKE2S_TYPE: LazyLock<usize> =
	LazyLock::new(|| hash_type_slot(HashKind::Blake2s.constructor_name()));

fn untag(object: *mut PyObject) -> *mut PyObject {
	crate::tag::untag_arg(object)
}

fn raise(kind: ExceptionKind, message: &str) -> *mut PyObject {
	crate::abi::exc::raise_kind_error_text(kind, message)
}

fn is_none(object: *mut PyObject) -> bool {
	// SAFETY: Type probe tolerates any live object.
	(unsafe { crate::types::dict::type_name(object) }) == Some("NoneType")
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

fn alloc_hash(kind: HashKind, data: Vec<u8>, blake2: Option<Blake2Params>) -> *mut PyObject {
	let object =
		Box::new(PyHash { ob_base: PyObjectHeader::new(hash_type(kind)), kind, data, blake2 });
	Box::into_raw(object).cast::<PyObject>()
}

unsafe fn receiver<'a>(object: *mut PyObject) -> Option<&'a mut PyHash> {
	let object = untag(object);
	if object.is_null() {
		return None;
	}
	// SAFETY: A non-NULL heap object carries a live header.
	if !is_hash_type(unsafe { (*object).ob_type }) {
		return None;
	}
	// SAFETY: Type check above proved the layout.
	Some(unsafe { &mut *object.cast::<PyHash>() })
}

fn arg_vec(argv: *mut *mut PyObject, argc: usize) -> Result<Vec<*mut PyObject>, *mut PyObject> {
	if argc == 0 {
		return Ok(Vec::new());
	}
	if argv.is_null() {
		return Err(raise(ExceptionKind::TypeError, "hash constructor argument vector is NULL"));
	}
	// SAFETY: Runtime builtin calling convention supplies `argc` live slots.
	let raw = unsafe { core::slice::from_raw_parts(argv, argc) };
	Ok(raw.iter().copied().map(untag).collect())
}

fn bytes_arg(object: *mut PyObject) -> Result<Vec<u8>, *mut PyObject> {
	bytes_like(object).map(<[u8]>::to_vec).ok_or_else(|| {
		raise(
			ExceptionKind::TypeError,
			&format!(
				"object supporting the buffer API required, not '{}'",
				// SAFETY: Type probe tolerates any live object.
				unsafe { crate::types::dict::type_name(object) }.unwrap_or("object")
			),
		)
	})
}

fn optional_bytes_arg(
	args: &[*mut PyObject],
	index: usize,
	default: &[u8],
	name: &str,
) -> Result<Vec<u8>, *mut PyObject> {
	match args.get(index).copied() {
		None => Ok(default.to_vec()),
		Some(value) if is_none(value) => Ok(default.to_vec()),
		Some(value) => bytes_like(value).map(<[u8]>::to_vec).ok_or_else(|| {
			raise(
				ExceptionKind::TypeError,
				&format!(
					"{name} must be bytes-like, not '{}'",
					// SAFETY: Type probe tolerates any live object.
					unsafe { crate::types::dict::type_name(value) }.unwrap_or("object")
				),
			)
		}),
	}
}

fn int_arg(object: *mut PyObject) -> Option<i64> {
	if crate::tag::is_small_int(object) {
		return Some(crate::tag::untag_small_int(object));
	}
	let object = untag(object);
	if object.is_null() {
		return None;
	}
	// SAFETY: Integer conversion type-checks the object.
	unsafe { crate::types::int::to_bigint_including_bool(object) }.and_then(|value| value.to_i64())
}

fn optional_usize_arg(
	args: &[*mut PyObject],
	index: usize,
	default: usize,
	name: &str,
) -> Result<usize, *mut PyObject> {
	match args.get(index).copied() {
		None => Ok(default),
		Some(value) if is_none(value) => Ok(default),
		Some(value) => {
			let Some(raw) = int_arg(value) else {
				return Err(raise(ExceptionKind::TypeError, &format!("{name} must be an integer")));
			};
			if raw < 0 {
				return Err(raise(ExceptionKind::ValueError, &format!("{name} must be non-negative")));
			}
			usize::try_from(raw)
				.map_err(|_| raise(ExceptionKind::OverflowError, &format!("{name} is too large")))
		},
	}
}

fn optional_bool_arg(
	args: &[*mut PyObject],
	index: usize,
	default: bool,
) -> Result<bool, *mut PyObject> {
	match args.get(index).copied() {
		None => Ok(default),
		Some(value) if is_none(value) => Ok(default),
		Some(value) => {
			// SAFETY: Truthiness helper follows the NULL-sentinel contract.
			match unsafe { abi::pon_is_true(value) } {
				0 => Ok(false),
				1 => Ok(true),
				_ => Err(ptr::null_mut()),
			}
		},
	}
}

fn unsupported_tree_param(
	args: &[*mut PyObject],
	index: usize,
	default: i64,
	name: &str,
) -> Result<(), *mut PyObject> {
	let Some(value) = args.get(index).copied() else {
		return Ok(());
	};
	if is_none(value) {
		return Ok(());
	}
	let Some(raw) = int_arg(value) else {
		return Err(raise(ExceptionKind::TypeError, &format!("{name} must be an integer")));
	};
	if raw == default {
		Ok(())
	} else {
		Err(raise(
			ExceptionKind::NotImplementedError,
			&format!("blake2 tree hashing parameter '{name}' is not implemented in pon"),
		))
	}
}

fn unsupported_last_node(args: &[*mut PyObject]) -> Result<(), *mut PyObject> {
	let Some(value) = args.get(11).copied() else {
		return Ok(());
	};
	if is_none(value) {
		return Ok(());
	}
	if optional_bool_arg(args, 11, false)? {
		Err(raise(
			ExceptionKind::NotImplementedError,
			"blake2 tree hashing parameter 'last_node' is not implemented in pon",
		))
	} else {
		Ok(())
	}
}

fn fixed_new(kind: HashKind, args: &[*mut PyObject]) -> *mut PyObject {
	if args.len() > 2 {
		return raise(
			ExceptionKind::TypeError,
			&format!("{}() takes at most 2 arguments ({} given)", kind.constructor_name(), args.len()),
		);
	}
	let mut data = Vec::new();
	if let Some(arg) = args.first().copied().filter(|&arg| !is_none(arg)) {
		match bytes_arg(arg) {
			Ok(payload) => data = payload,
			Err(raised) => return raised,
		}
	}
	alloc_hash(kind, data, None)
}

fn blake2_new(kind: HashKind, args: &[*mut PyObject]) -> *mut PyObject {
	if args.len() > 1 && args.len() != BLAKE2_ARG_SLOTS {
		return raise(
			ExceptionKind::TypeError,
			&format!(
				"{}() takes at most 1 positional argument ({} given)",
				kind.constructor_name(),
				args.len()
			),
		);
	}

	let mut data = Vec::new();
	if let Some(arg) = args.first().copied().filter(|&arg| !is_none(arg)) {
		match bytes_arg(arg) {
			Ok(payload) => data = payload,
			Err(raised) => return raised,
		}
	}

	let max_digest = kind.default_digest_size();
	let mut params = Blake2Params::default_for(kind);
	params.digest_size = match optional_usize_arg(args, 1, max_digest, "digest_size") {
		Ok(value) if (1..=max_digest).contains(&value) => value,
		Ok(_) => {
			return raise(
				ExceptionKind::ValueError,
				&format!("digest_size must be between 1 and {max_digest} bytes"),
			);
		},
		Err(raised) => return raised,
	};

	params.key = match optional_bytes_arg(args, 2, &[], "key") {
		Ok(value) => value,
		Err(raised) => return raised,
	};
	let max_key = kind.block_size() as usize;
	if params.key.len() > max_key {
		return raise(ExceptionKind::ValueError, &format!("maximum key length is {max_key} bytes"));
	}

	params.salt = match optional_bytes_arg(args, 3, &[], "salt") {
		Ok(value) => value,
		Err(raised) => return raised,
	};
	params.person = match optional_bytes_arg(args, 4, &[], "person") {
		Ok(value) => value,
		Err(raised) => return raised,
	};
	let max_person = if kind == HashKind::Blake2b {
		BLAKE2B_PERSON_SIZE
	} else {
		BLAKE2S_PERSON_SIZE
	};
	if params.salt.len() > max_person {
		return raise(
			ExceptionKind::ValueError,
			&format!("maximum salt length is {max_person} bytes"),
		);
	}
	if params.person.len() > max_person {
		return raise(
			ExceptionKind::ValueError,
			&format!("maximum person length is {max_person} bytes"),
		);
	}

	for (index, default, name) in [
		(5, 1, "fanout"),
		(6, 1, "depth"),
		(7, 0, "leaf_size"),
		(8, 0, "node_offset"),
		(9, 0, "node_depth"),
		(10, 0, "inner_size"),
	] {
		if let Err(raised) = unsupported_tree_param(args, index, default, name) {
			return raised;
		}
	}
	if let Err(raised) = unsupported_last_node(args) {
		return raised;
	}

	alloc_hash(kind, data, Some(params))
}

fn hash_new(kind: HashKind, argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
	let args = match arg_vec(argv, argc) {
		Ok(args) => args,
		Err(raised) => return raised,
	};
	if kind.is_blake2() {
		blake2_new(kind, &args)
	} else {
		fixed_new(kind, &args)
	}
}

macro_rules! hash_constructor {
	($entry:ident, $kind:expr) => {
		unsafe extern "C" fn $entry(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
			hash_new($kind, argv, argc)
		}
	};
}

hash_constructor!(md5_new, HashKind::Md5);
hash_constructor!(sha1_new, HashKind::Sha1);
hash_constructor!(sha3_224_new, HashKind::Sha3_224);
hash_constructor!(sha3_256_new, HashKind::Sha3_256);
hash_constructor!(sha3_384_new, HashKind::Sha3_384);
hash_constructor!(sha3_512_new, HashKind::Sha3_512);
hash_constructor!(shake_128_new, HashKind::Shake128);
hash_constructor!(shake_256_new, HashKind::Shake256);
hash_constructor!(blake2b_new, HashKind::Blake2b);
hash_constructor!(blake2s_new, HashKind::Blake2s);

macro_rules! hash_method {
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
					concat!($name, "() receiver must be a hash object"),
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
	const HEX: &[u8; 16] = b"0123456789abcdef";
	let mut out = Vec::with_capacity(digest.len() * 2);
	for &byte in digest {
		out.push(HEX[usize::from(byte >> 4)]);
		out.push(HEX[usize::from(byte & 0x0f)]);
	}
	// SAFETY: Only ASCII hex digits were written.
	unsafe { String::from_utf8_unchecked(out) }
}

fn fixed_digest(kind: HashKind, data: &[u8]) -> Vec<u8> {
	match kind {
		HashKind::Md5 => ::md5::Md5::digest(data).to_vec(),
		HashKind::Sha1 => ::sha1::Sha1::digest(data).to_vec(),
		HashKind::Sha3_224 => ::sha3::Sha3_224::digest(data).to_vec(),
		HashKind::Sha3_256 => ::sha3::Sha3_256::digest(data).to_vec(),
		HashKind::Sha3_384 => ::sha3::Sha3_384::digest(data).to_vec(),
		HashKind::Sha3_512 => ::sha3::Sha3_512::digest(data).to_vec(),
		HashKind::Blake2b | HashKind::Blake2s | HashKind::Shake128 | HashKind::Shake256 => Vec::new(),
	}
}

fn shake_digest(kind: HashKind, data: &[u8], length: usize) -> Vec<u8> {
	let mut out = vec![0; length];
	match kind {
		HashKind::Shake128 => {
			let mut hasher = ::sha3::Shake128::default();
			hasher.update(data);
			let mut reader = hasher.finalize_xof();
			reader.read(&mut out);
		},
		HashKind::Shake256 => {
			let mut hasher = ::sha3::Shake256::default();
			hasher.update(data);
			let mut reader = hasher.finalize_xof();
			reader.read(&mut out);
		},
		_ => {},
	}
	out
}

fn blake2b_digest(data: &[u8], params: &Blake2Params) -> Vec<u8> {
	let core = ::blake2::Blake2bVarCore::new_with_params(
		&params.salt,
		&params.person,
		params.key.len(),
		params.digest_size,
	);
	let mut hasher = CoreWrapper::from_core(core);
	if !params.key.is_empty() {
		let mut block = [0u8; BLAKE2B_BLOCK_SIZE];
		block[..params.key.len()].copy_from_slice(&params.key);
		hasher.update(&block);
	}
	hasher.update(data);
	let (mut core, mut buffer) = hasher.decompose();
	let mut full = Output::<::blake2::Blake2bVarCore>::default();
	core.finalize_variable_core(&mut buffer, &mut full);
	full[..params.digest_size].to_vec()
}

fn blake2s_digest(data: &[u8], params: &Blake2Params) -> Vec<u8> {
	let core = ::blake2::Blake2sVarCore::new_with_params(
		&params.salt,
		&params.person,
		params.key.len(),
		params.digest_size,
	);
	let mut hasher = CoreWrapper::from_core(core);
	if !params.key.is_empty() {
		let mut block = [0u8; BLAKE2S_BLOCK_SIZE];
		block[..params.key.len()].copy_from_slice(&params.key);
		hasher.update(&block);
	}
	hasher.update(data);
	let (mut core, mut buffer) = hasher.decompose();
	let mut full = Output::<::blake2::Blake2sVarCore>::default();
	core.finalize_variable_core(&mut buffer, &mut full);
	full[..params.digest_size].to_vec()
}

fn current_digest(this: &PyHash) -> Vec<u8> {
	match this.kind {
		HashKind::Blake2b => {
			blake2b_digest(&this.data, this.blake2.as_ref().expect("blake2b params"))
		},
		HashKind::Blake2s => {
			blake2s_digest(&this.data, this.blake2.as_ref().expect("blake2s params"))
		},
		HashKind::Shake128 | HashKind::Shake256 => Vec::new(),
		_ => fixed_digest(this.kind, &this.data),
	}
}

fn shake_length_arg(
	kind: HashKind,
	args: &[*mut PyObject],
	method_name: &str,
) -> Result<usize, *mut PyObject> {
	if args.len() != 1 {
		return Err(raise(
			ExceptionKind::TypeError,
			&format!("{}() missing required argument 'length'", method_name),
		));
	}
	let Some(length) = int_arg(args[0]) else {
		return Err(raise(ExceptionKind::TypeError, "length must be an integer"));
	};
	if length < 0 {
		return Err(raise(ExceptionKind::ValueError, "length must be non-negative"));
	}
	usize::try_from(length).map_err(|_| {
		raise(
			ExceptionKind::OverflowError,
			&format!("{} length is too large", kind.constructor_name()),
		)
	})
}

hash_method!(hash_update, "update", |this: &mut PyHash, args: &[*mut PyObject]| {
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

hash_method!(hash_digest_method, "digest", |this: &mut PyHash, args: &[*mut PyObject]| {
	if this.kind.is_shake() {
		let length = match shake_length_arg(this.kind, args, "digest") {
			Ok(length) => length,
			Err(raised) => return raised,
		};
		return pon_bytes_from_digest(&shake_digest(this.kind, &this.data, length));
	}
	if !args.is_empty() {
		return raise(ExceptionKind::TypeError, "digest() takes no arguments");
	}
	pon_bytes_from_digest(&current_digest(this))
});

hash_method!(hash_hexdigest, "hexdigest", |this: &mut PyHash, args: &[*mut PyObject]| {
	let digest = if this.kind.is_shake() {
		let length = match shake_length_arg(this.kind, args, "hexdigest") {
			Ok(length) => length,
			Err(raised) => return raised,
		};
		shake_digest(this.kind, &this.data, length)
	} else {
		if !args.is_empty() {
			return raise(ExceptionKind::TypeError, "hexdigest() takes no arguments");
		}
		current_digest(this)
	};
	let hex = hex_digest(&digest);
	// SAFETY: Runtime allocation helper; NULL on failure with the error set.
	unsafe { abi::pon_const_str(hex.as_ptr(), hex.len()) }
});

hash_method!(hash_copy, "copy", |this: &mut PyHash, args: &[*mut PyObject]| {
	if !args.is_empty() {
		return raise(ExceptionKind::TypeError, "copy() takes no arguments");
	}
	alloc_hash(this.kind, this.data.clone(), this.blake2.clone())
});

unsafe extern "C" fn hash_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
	let name = untag(name);
	// SAFETY: `unicode_text` type-checks its argument.
	let Some(name_text) = (unsafe { crate::types::type_::unicode_text(name) }) else {
		crate::thread_state::pon_err_set("attribute name must be str");
		return ptr::null_mut();
	};
	let Some(this) = (unsafe { receiver(object) }) else {
		return raise(ExceptionKind::TypeError, "hash getattro on non-hash receiver");
	};
	let entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject = match name_text {
		// SAFETY: Runtime allocation helpers follow the NULL-sentinel contract.
		"name" => {
			let constructor_name = this.kind.constructor_name();
			return unsafe { abi::pon_const_str(constructor_name.as_ptr(), constructor_name.len()) };
		},
		// SAFETY: Same contract as above.
		"digest_size" => return unsafe { abi::pon_const_int(this.digest_size() as i64) },
		// SAFETY: Same contract as above.
		"block_size" => return unsafe { abi::pon_const_int(this.kind.block_size()) },
		"update" => hash_update,
		"digest" => hash_digest_method,
		"hexdigest" => hash_hexdigest,
		"copy" => hash_copy,
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

fn constructor_entry(
	kind: HashKind,
) -> unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject {
	match kind {
		HashKind::Md5 => md5_new,
		HashKind::Sha1 => sha1_new,
		HashKind::Sha3_224 => sha3_224_new,
		HashKind::Sha3_256 => sha3_256_new,
		HashKind::Sha3_384 => sha3_384_new,
		HashKind::Sha3_512 => sha3_512_new,
		HashKind::Shake128 => shake_128_new,
		HashKind::Shake256 => shake_256_new,
		HashKind::Blake2b => blake2b_new,
		HashKind::Blake2s => blake2s_new,
	}
}

fn int_attr(name: &str, value: i64) -> Result<(u32, *mut PyObject), String> {
	let object = unsafe { abi::pon_const_int(value) };
	if object.is_null() {
		return Err(format!("failed to allocate hash module {name}"));
	}
	Ok((intern(name), object))
}

fn module_constructor(kind: HashKind) -> Result<*mut PyObject, String> {
	let name = kind.constructor_name();
	// SAFETY: Live builtin entry point with the runtime calling convention.
	let constructor = unsafe {
		abi::pon_make_function(constructor_entry(kind) as *const u8, VARIADIC_ARITY, intern(name))
	};
	if constructor.is_null() {
		return Err(format!("failed to allocate hash constructor {name}"));
	}
	Ok(constructor)
}

fn make_hash_module(name: &str, kinds: &[HashKind]) -> Result<*mut PyObject, String> {
	// SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
	let name_object = unsafe { abi::pon_const_str(name.as_ptr(), name.len()) };
	if name_object.is_null() {
		return Err(format!("failed to allocate {name}.__name__"));
	}
	let mut attrs = vec![(intern("__name__"), name_object)];
	for &kind in kinds {
		attrs.push((intern(kind.constructor_name()), module_constructor(kind)?));
	}
	attrs.push(int_attr("_GIL_MINSIZE", HASHLIB_GIL_MINSIZE)?);
	match name {
		"_md5" => attrs.push((intern("MD5Type"), hash_type(HashKind::Md5).cast::<PyObject>())),
		"_sha1" => attrs.push((intern("SHA1Type"), hash_type(HashKind::Sha1).cast::<PyObject>())),
		"_blake2" => {
			for &(const_name, value) in &[
				("BLAKE2B_MAX_DIGEST_SIZE", BLAKE2B_MAX_DIGEST_SIZE as i64),
				("BLAKE2B_MAX_KEY_SIZE", BLAKE2B_MAX_KEY_SIZE as i64),
				("BLAKE2B_PERSON_SIZE", BLAKE2B_PERSON_SIZE as i64),
				("BLAKE2B_SALT_SIZE", BLAKE2B_PERSON_SIZE as i64),
				("BLAKE2S_MAX_DIGEST_SIZE", BLAKE2S_MAX_DIGEST_SIZE as i64),
				("BLAKE2S_MAX_KEY_SIZE", BLAKE2S_MAX_KEY_SIZE as i64),
				("BLAKE2S_PERSON_SIZE", BLAKE2S_PERSON_SIZE as i64),
				("BLAKE2S_SALT_SIZE", BLAKE2S_PERSON_SIZE as i64),
			] {
				attrs.push(int_attr(const_name, value)?);
			}
		},
		_ => {},
	}
	install_module(name, attrs)
}

pub(super) fn make_md5_module() -> Result<*mut PyObject, String> {
	make_hash_module("_md5", &HashKind::MD5)
}

pub(super) fn make_sha1_module() -> Result<*mut PyObject, String> {
	make_hash_module("_sha1", &HashKind::SHA1)
}

pub(super) fn make_sha3_module() -> Result<*mut PyObject, String> {
	make_hash_module("_sha3", &HashKind::SHA3)
}

pub(super) fn make_blake2_module() -> Result<*mut PyObject, String> {
	make_hash_module("_blake2", &HashKind::BLAKE2)
}
