//! CPython `pyhash.c` parity: the SipHash-1-3 core plus the `str`/`bytes`
//! hash functions built on it.
//!
//! CPython 3.14 hashes `str` and `bytes` with `pysiphash13` keyed by
//! `_Py_HashSecret.siphash.{k0,k1}`.  The conformance harness pins
//! `PYTHONHASHSEED=0`, under which CPython leaves `_Py_HashSecret` all
//! zeros, so seed-0 parity means exactly `siphash13(k0=0, k1=0, data)`
//! plus `_Py_HashBytes`'s edge rules (empty input hashes to `0`; a raw
//! result of `-1` is remapped to `-2` because `-1` is the C-level error
//! sentinel).
//!
//! `str` hashing has one further wrinkle: CPython hashes the PEP 393
//! canonical buffer (UCS1/UCS2/UCS4 chosen by the widest code point), not
//! UTF-8.  pon stores UTF-8, so [`str_hash`] re-encodes non-ASCII text into
//! the canonical width before hashing.  Multi-byte code units are laid out
//! little-endian, which matches CPython's in-memory representation on every
//! target pon supports (aarch64/x86_64 are little-endian; CPython's hash of
//! a wide string is endianness-dependent by design).
//!
//! One hash universe: dict keys, set/frozenset elements, and the
//! user-visible `hash()` builtin all route through these functions for
//! `str`/`bytes`, so bucket placement and observable values can never
//! disagree.  (No separate fast path for interned-name lookups: dict entries
//! cache their key hash at insert, so lookups pay one hash per probe string
//! either way, and the bench gate showed no regression worth a split.)

/// SipHash-1-3 exactly as CPython's `pyhash.c` `siphash13`: also the
/// `_Py_KeyedHash` used by `_imp.source_hash` (k0 = key, k1 = 0).
#[must_use]
pub fn siphash13(k0: u64, k1: u64, data: &[u8]) -> u64 {
    #[inline]
    fn single_round(v: &mut [u64; 4]) {
        // HALF_ROUND(v0, v1, v2, v3, 13, 16)
        v[0] = v[0].wrapping_add(v[1]);
        v[2] = v[2].wrapping_add(v[3]);
        v[1] = v[1].rotate_left(13) ^ v[0];
        v[3] = v[3].rotate_left(16) ^ v[2];
        v[0] = v[0].rotate_left(32);
        // HALF_ROUND(v2, v1, v0, v3, 17, 21)
        v[2] = v[2].wrapping_add(v[1]);
        v[0] = v[0].wrapping_add(v[3]);
        v[1] = v[1].rotate_left(17) ^ v[2];
        v[3] = v[3].rotate_left(21) ^ v[0];
        v[2] = v[2].rotate_left(32);
    }

    let mut v = [
        k0 ^ 0x736f_6d65_7073_6575,
        k1 ^ 0x646f_7261_6e64_6f6d,
        k0 ^ 0x6c79_6765_6e65_7261,
        k1 ^ 0x7465_6462_7974_6573,
    ];
    let mut b = (data.len() as u64) << 56;
    let mut chunks = data.chunks_exact(8);
    for chunk in &mut chunks {
        let mi = u64::from_le_bytes(chunk.try_into().expect("8-byte chunk"));
        v[3] ^= mi;
        single_round(&mut v);
        v[0] ^= mi;
    }
    let tail = chunks.remainder();
    let mut t = [0u8; 8];
    t[..tail.len()].copy_from_slice(tail);
    b |= u64::from_le_bytes(t);

    v[3] ^= b;
    single_round(&mut v);
    v[0] ^= b;
    v[2] ^= 0xff;
    single_round(&mut v);
    single_round(&mut v);
    single_round(&mut v);
    (v[0] ^ v[1]) ^ (v[2] ^ v[3])
}

/// CPython `_Py_HashBytes` under `PYTHONHASHSEED=0`: `bytes.__hash__` and
/// the byte-buffer half of `str.__hash__`.
#[must_use]
pub fn bytes_hash(data: &[u8]) -> i64 {
    if data.is_empty() {
        return 0;
    }
    let hash = siphash13(0, 0, data) as i64;
    if hash == -1 { -2 } else { hash }
}

/// CPython `unicode_hash` under `PYTHONHASHSEED=0`: hashes the PEP 393
/// canonical buffer (UCS1 for max code point < U+0100, else UCS2, else
/// UCS4; multi-byte units little-endian) via [`bytes_hash`].
#[must_use]
pub fn str_hash(text: &str) -> i64 {
    if text.is_empty() {
        return 0;
    }
    // ASCII is its own UCS1 buffer: hash the UTF-8 bytes in place.
    if text.is_ascii() {
        return bytes_hash(text.as_bytes());
    }
    let max = text.chars().map(|ch| ch as u32).max().unwrap_or(0);
    let buffer: Vec<u8> = if max < 0x100 {
        // Latin-1: one byte per code point (lossless below U+0100).
        text.chars().map(|ch| ch as u32 as u8).collect()
    } else if max < 0x1_0000 {
        text.chars().flat_map(|ch| (ch as u32 as u16).to_le_bytes()).collect()
    } else {
        text.chars().flat_map(|ch| (ch as u32).to_le_bytes()).collect()
    };
    bytes_hash(&buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Oracle: `PYTHONHASHSEED=0 python3.14` (siphash13), one vector per
    /// PEP 393 width plus the `_Py_HashBytes` edge rules.
    #[test]
    fn str_hash_matches_cpython_seed0_vectors() {
        let cases: &[(&str, i64)] = &[
            ("", 0),
            ("a", 4644417185603328019),
            ("abc", -4594863902769663758),
            // UCS1, non-ASCII: 'é' = U+00E9 hashes as Latin-1, not UTF-8.
            ("abcé", -2884356235179635801),
            ("né", 6145485207905905858),
            // UCS2: 'Δ' = U+0394.
            ("\u{0394}", 7175442150425155517),
            ("a\u{0394}", -3027069262285291068),
            // UCS4: U+1F600.
            ("\u{1F600}", -3536540696076613844),
        ];
        for (text, expected) in cases {
            assert_eq!(str_hash(text), *expected, "hash({text:?})");
        }
        let long = "x".repeat(64);
        assert_eq!(str_hash(&long), 5471797116534828707, "hash('x' * 64)");
    }

    #[test]
    fn bytes_hash_matches_cpython_seed0_vectors() {
        let cases: &[(&[u8], i64)] = &[
            (b"", 0),
            (b"a", 4644417185603328019),
            (b"bytes", -5534508902283672202),
            (b"abc\xff", -1222198482973350627),
        ];
        for (data, expected) in cases {
            assert_eq!(bytes_hash(data), *expected, "hash({data:?})");
        }
    }

    /// An ASCII str hashes exactly like its ASCII bytes (same UCS1 buffer).
    #[test]
    fn ascii_str_and_bytes_share_hashes() {
        assert_eq!(str_hash("bytes"), bytes_hash(b"bytes"));
    }
}
