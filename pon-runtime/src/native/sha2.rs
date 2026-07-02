//! Native `_sha2` module: SHA-512 (HANDOFF Track L).
//!
//! `random.py` imports `sha512` at module top (`from _sha2 import sha512`)
//! and feeds `_sha512(a).digest()` into `Random.seed` for str/bytes seeds,
//! so the digest must be the real FIPS 180-4 SHA-512 for seeded sequences to
//! match CPython. The hash object buffers its input and computes on demand;
//! the surface is the digest-object subset the stdlib chain consumes
//! (`update`/`digest`/`hexdigest`/`copy` plus the standard metadata attrs).

use std::ptr;
use std::sync::LazyLock;

use crate::abi;
use crate::intern::intern;
use crate::object::{PyObject, PyObjectHeader, PyType};
use crate::types::exc::ExceptionKind;
use crate::types::{bytearray_ as bytearray_type, bytes_ as bytes_type};

use super::builtins_mod::VARIADIC_ARITY;
use super::install_module;

// FIPS 180-4 SHA-512 initial hash value.
const H512: [u64; 8] = [
    0x6a09e667f3bcc908,
    0xbb67ae8584caa73b,
    0x3c6ef372fe94f82b,
    0xa54ff53a5f1d36f1,
    0x510e527fade682d1,
    0x9b05688c2b3e6c1f,
    0x1f83d9abfb41bd6b,
    0x5be0cd19137e2179,
];

// FIPS 180-4 SHA-512 round constants.
#[rustfmt::skip]
const K512: [u64; 80] = [
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc,
    0x3956c25bf348b538, 0x59f111f1b605d019, 0x923f82a4af194f9b, 0xab1c5ed5da6d8118,
    0xd807aa98a3030242, 0x12835b0145706fbe, 0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235, 0xc19bf174cf692694,
    0xe49b69c19ef14ad2, 0xefbe4786384f25e3, 0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65,
    0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5,
    0x983e5152ee66dfab, 0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2, 0xd5a79147930aa725, 0x06ca6351e003826f, 0x142929670a0e6e70,
    0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed, 0x53380d139d95b3df,
    0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b,
    0xa2bfe8a14cf10364, 0xa81a664bbc423001, 0xc24b8b70d0f89791, 0xc76c51a30654be30,
    0xd192e819d6ef5218, 0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8, 0x1e376c085141ab53, 0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb, 0x5b9cca4f7763e373, 0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec,
    0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b,
    0xca273eceea26619c, 0xd186b8c721c0c207, 0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178,
    0x06f067aa72176fba, 0x0a637dc5a2c898a6, 0x113f9804bef90dae, 0x1b710b35131c471b,
    0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc, 0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6, 0x597f299cfc657e2a, 0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
];

/// FIPS 180-4 SHA-512 over the full message (single-shot; callers buffer).
fn sha512_digest(message: &[u8]) -> [u8; 64] {
    let mut h = H512;
    // Padding: 0x80, zeros, 128-bit big-endian bit length.
    let mut padded = message.to_vec();
    padded.push(0x80);
    while padded.len() % 128 != 112 {
        padded.push(0);
    }
    let bit_len = (message.len() as u128) * 8;
    padded.extend_from_slice(&bit_len.to_be_bytes());

    let mut w = [0u64; 80];
    for block in padded.chunks_exact(128) {
        for (t, chunk) in block.chunks_exact(8).enumerate() {
            w[t] = u64::from_be_bytes(chunk.try_into().expect("8-byte chunk"));
        }
        for t in 16..80 {
            let s0 = w[t - 15].rotate_right(1) ^ w[t - 15].rotate_right(8) ^ (w[t - 15] >> 7);
            let s1 = w[t - 2].rotate_right(19) ^ w[t - 2].rotate_right(61) ^ (w[t - 2] >> 6);
            w[t] = w[t - 16].wrapping_add(s0).wrapping_add(w[t - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for t in 0..80 {
            let big_s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(big_s1)
                .wrapping_add(ch)
                .wrapping_add(K512[t])
                .wrapping_add(w[t]);
            let big_s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = big_s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, value) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(value);
        }
    }
    let mut out = [0u8; 64];
    for (chunk, value) in out.chunks_exact_mut(8).zip(h) {
        chunk.copy_from_slice(&value.to_be_bytes());
    }
    out
}

// ---------------------------------------------------------------------------
// Hash object

#[repr(C)]
struct PySha512 {
    ob_base: PyObjectHeader,
    /// Buffered message; the digest is computed on demand.
    data: Vec<u8>,
}

static SHA512_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(abi::runtime_type_type().cast_const(), "sha512", core::mem::size_of::<PySha512>());
    ty.tp_getattro = Some(sha512_getattro);
    Box::into_raw(Box::new(ty)) as usize
});

fn sha512_type() -> *mut PyType {
    *SHA512_TYPE as *mut PyType
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

fn alloc_sha512(data: Vec<u8>) -> *mut PyObject {
    let object = Box::new(PySha512 { ob_base: PyObjectHeader::new(sha512_type()), data });
    Box::into_raw(object).cast::<PyObject>()
}

unsafe fn receiver<'a>(object: *mut PyObject) -> Option<&'a mut PySha512> {
    let object = untag(object);
    if object.is_null() {
        return None;
    }
    // SAFETY: A non-NULL heap object carries a live header.
    if unsafe { (*object).ob_type } != sha512_type().cast_const() {
        return None;
    }
    // SAFETY: Type check above proved the layout.
    Some(unsafe { &mut *object.cast::<PySha512>() })
}

/// `sha512(data=b'', *, usedforsecurity=True)` module constructor.
unsafe extern "C" fn sha512_new(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    if argc > 2 {
        return raise(
            ExceptionKind::TypeError,
            &format!("sha512() takes at most 2 arguments ({argc} given)"),
        );
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
    alloc_sha512(data)
}

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
                return raise(ExceptionKind::TypeError, concat!($name, "() receiver must be a sha512 object"));
            };
            let args: Vec<*mut PyObject> = raw[1..].iter().copied().map(untag).collect();
            #[allow(clippy::redundant_closure_call)]
            ($body)(this, &args)
        }
    };
}

sha_method!(sha512_update, "update", |this: &mut PySha512, args: &[*mut PyObject]| {
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

sha_method!(sha512_digest_method, "digest", |this: &mut PySha512, args: &[*mut PyObject]| {
    if !args.is_empty() {
        return raise(ExceptionKind::TypeError, "digest() takes no arguments");
    }
    let digest = sha512_digest(&this.data);
    // SAFETY: Runtime allocation helper; NULL on failure with the error set.
    unsafe { abi::str_::pon_const_bytes(digest.as_ptr(), digest.len()) }
});

sha_method!(sha512_hexdigest, "hexdigest", |this: &mut PySha512, args: &[*mut PyObject]| {
    if !args.is_empty() {
        return raise(ExceptionKind::TypeError, "hexdigest() takes no arguments");
    }
    let digest = sha512_digest(&this.data);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    // SAFETY: Runtime allocation helper; NULL on failure with the error set.
    unsafe { abi::pon_const_str(hex.as_ptr(), hex.len()) }
});

sha_method!(sha512_copy, "copy", |this: &mut PySha512, args: &[*mut PyObject]| {
    if !args.is_empty() {
        return raise(ExceptionKind::TypeError, "copy() takes no arguments");
    }
    alloc_sha512(this.data.clone())
});

unsafe extern "C" fn sha512_getattro(object: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    let name = untag(name);
    // SAFETY: `unicode_text` type-checks its argument.
    let Some(name_text) = (unsafe { crate::types::type_::unicode_text(name) }) else {
        crate::thread_state::pon_err_set("attribute name must be str");
        return ptr::null_mut();
    };
    let entry: unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject = match name_text {
        // SAFETY: Runtime allocation helpers follow the NULL-sentinel contract.
        "name" => return unsafe { abi::pon_const_str("sha512".as_ptr(), "sha512".len()) },
        // SAFETY: Same contract as above.
        "digest_size" => return unsafe { abi::pon_const_int(64) },
        // SAFETY: Same contract as above.
        "block_size" => return unsafe { abi::pon_const_int(128) },
        "update" => sha512_update,
        "digest" => sha512_digest_method,
        "hexdigest" => sha512_hexdigest,
        "copy" => sha512_copy,
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

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let name = "_sha2";
    // SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
    let name_object = unsafe { abi::pon_const_str(name.as_ptr(), name.len()) };
    if name_object.is_null() {
        return Err("failed to allocate _sha2.__name__".to_owned());
    }
    // SAFETY: Live builtin entry point with the runtime calling convention.
    let constructor = unsafe { abi::pon_make_function(sha512_new as *const u8, VARIADIC_ARITY, intern("sha512")) };
    if constructor.is_null() {
        return Err("failed to allocate _sha2.sha512".to_owned());
    }
    let attrs = vec![(intern("__name__"), name_object), (intern("sha512"), constructor)];
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
