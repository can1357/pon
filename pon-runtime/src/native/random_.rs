//! Native `_random` module: the MT19937 core generator (HANDOFF Track L).
//!
//! `random.Random` subclasses `_random.Random`, so the type publishes its
//! method surface through `tp_dict` (the `ensure_dict_subclass_methods`
//! pattern) where the heap-class MRO walk finds it: `super().seed(a)` and
//! inherited `random()`/`getrandbits()` bind these native entries on the
//! generic heap-instance `self`. Generator state lives in an address-keyed
//! side table (the `types::int::BIG_INTS` convention), created on demand
//! with OS-entropy seeding exactly like CPython's `random_new`.
//!
//! The twister core is the verbatim MT19937 algorithm from
//! `Modules/_randommodule.c` (init_genrand / init_by_array / genrand_uint32),
//! so seeded sequences are bit-identical to CPython.

use std::collections::HashMap;
use std::ptr;
use std::sync::{LazyLock, Mutex};

use num_bigint::{BigInt, Sign};
use num_traits::{Signed, ToPrimitive, Zero};

use crate::abi;
use crate::intern::intern;
use crate::object::{PyObject, PyType};
use crate::types::type_::PyHeapInstance;
use crate::types::exc::ExceptionKind;
use crate::types::int::{from_bigint, to_bigint_including_bool};

use super::builtins_mod::VARIADIC_ARITY;
use super::install_module;

const N: usize = 624;
const M: usize = 397;
const MATRIX_A: u32 = 0x9908_b0df;
const UPPER_MASK: u32 = 0x8000_0000;
const LOWER_MASK: u32 = 0x7fff_ffff;

struct Mt19937 {
    mt: [u32; N],
    mti: usize,
}

impl Mt19937 {
    /// `init_genrand`: seeds the state array with one 32-bit seed.
    fn init_genrand(&mut self, s: u32) {
        self.mt[0] = s;
        for i in 1..N {
            let prev = self.mt[i - 1];
            self.mt[i] = 1_812_433_253u32.wrapping_mul(prev ^ (prev >> 30)).wrapping_add(i as u32);
        }
        self.mti = N;
    }

    /// `init_by_array`: seeds from an arbitrary-length 32-bit key.
    fn init_by_array(&mut self, key: &[u32]) {
        self.init_genrand(19_650_218);
        let mut i = 1usize;
        let mut j = 0usize;
        let mut k = N.max(key.len());
        while k > 0 {
            let prev = self.mt[i - 1];
            self.mt[i] = (self.mt[i] ^ (prev ^ (prev >> 30)).wrapping_mul(1_664_525))
                .wrapping_add(key[j])
                .wrapping_add(j as u32);
            i += 1;
            j += 1;
            if i >= N {
                self.mt[0] = self.mt[N - 1];
                i = 1;
            }
            if j >= key.len() {
                j = 0;
            }
            k -= 1;
        }
        k = N - 1;
        while k > 0 {
            let prev = self.mt[i - 1];
            self.mt[i] =
                (self.mt[i] ^ (prev ^ (prev >> 30)).wrapping_mul(1_566_083_941)).wrapping_sub(i as u32);
            i += 1;
            if i >= N {
                self.mt[0] = self.mt[N - 1];
                i = 1;
            }
            k -= 1;
        }
        self.mt[0] = 0x8000_0000; // MSB is 1, assuring a non-zero initial array
    }

    /// `genrand_uint32`: one tempered 32-bit word.
    fn genrand(&mut self) -> u32 {
        if self.mti >= N {
            for kk in 0..N - M {
                let y = (self.mt[kk] & UPPER_MASK) | (self.mt[kk + 1] & LOWER_MASK);
                self.mt[kk] = self.mt[kk + M] ^ (y >> 1) ^ if y & 1 == 1 { MATRIX_A } else { 0 };
            }
            for kk in N - M..N - 1 {
                let y = (self.mt[kk] & UPPER_MASK) | (self.mt[kk + 1] & LOWER_MASK);
                self.mt[kk] = self.mt[kk + M - N] ^ (y >> 1) ^ if y & 1 == 1 { MATRIX_A } else { 0 };
            }
            let y = (self.mt[N - 1] & UPPER_MASK) | (self.mt[0] & LOWER_MASK);
            self.mt[N - 1] = self.mt[M - 1] ^ (y >> 1) ^ if y & 1 == 1 { MATRIX_A } else { 0 };
            self.mti = 0;
        }
        let mut y = self.mt[self.mti];
        self.mti += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^ (y >> 18)
    }

    /// `random_random`: 53-bit double in [0, 1).
    fn random(&mut self) -> f64 {
        let a = self.genrand() >> 5; // 27 bits
        let b = self.genrand() >> 6; // 26 bits
        (f64::from(a) * 67_108_864.0 + f64::from(b)) * (1.0 / 9_007_199_254_740_992.0)
    }

    /// Fresh generator seeded from OS entropy (CPython `random_seed(NULL)`).
    fn from_entropy() -> Self {
        let mut state = Self { mt: [0; N], mti: N + 1 };
        let mut key = [0u32; 16];
        // SAFETY: `key` is a live writable 64-byte buffer (within the
        // getentropy 256-byte call limit).
        if unsafe { libc::getentropy(key.as_mut_ptr().cast(), core::mem::size_of_val(&key)) } != 0 {
            // Entropy source unavailable: degrade to the address/time mix
            // CPython uses in its own fallback path.
            let clock = std::time::UNIX_EPOCH.elapsed().map_or(0, |d| d.as_nanos() as u64);
            key[0] = clock as u32;
            key[1] = (clock >> 32) as u32;
        }
        state.init_by_array(&key);
        state
    }
}

/// Generator state per `Random` instance (exact `_random.Random` objects and
/// heap-subclass instances alike), keyed by object address. Objects are
/// runtime-owned allocations; stale entries persist harmlessly.
static STATES: LazyLock<Mutex<HashMap<usize, Mt19937>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Runs `body` on the receiver's generator, seeding from OS entropy on first
/// touch (CPython seeds in `random_new`; the side table defers that to first
/// use, which is observably identical).
fn with_state<R>(receiver: *mut PyObject, body: impl FnOnce(&mut Mt19937) -> R) -> R {
    let mut states = STATES.lock().unwrap_or_else(|poison| poison.into_inner());
    let state = states.entry(receiver as usize).or_insert_with(Mt19937::from_entropy);
    body(state)
}

// ---------------------------------------------------------------------------
// Module factory

pub(super) fn make_module() -> Result<*mut PyObject, String> {
    let name = "_random";
    // SAFETY: Runtime allocation helpers return NULL with a diagnostic on failure.
    let name_object = unsafe { abi::pon_const_str(name.as_ptr(), name.len()) };
    if name_object.is_null() {
        return Err("failed to allocate _random.__name__".to_owned());
    }
    let attrs = vec![
        (intern("__name__"), name_object),
        (intern("Random"), random_type().cast::<PyObject>()),
    ];
    install_module(name, attrs)
}

static RANDOM_TYPE: LazyLock<usize> = LazyLock::new(|| {
    let mut ty = PyType::new(
        abi::runtime_type_type().cast_const(),
        "Random",
        core::mem::size_of::<PyHeapInstance>(),
    );
    ty.tp_base = abi::runtime_global(intern("object")).map_or(ptr::null_mut(), |object| object.cast::<PyType>());
    // Exact `_random.Random()` instances resolve methods through the same
    // MRO walk heap subclasses use (test_random exercises the raw type).
    ty.tp_getattro = Some(crate::descr::generic_get_attr);
    // Exact-type construction seeds from the ctor argument (`random_new`).
    ty.tp_new = Some(random_new);
    let namespace = crate::types::type_::new_namespace();
    let methods: &[(&str, *const u8)] = &[
        ("seed", random_seed as *const u8),
        ("random", random_random as *const u8),
        ("getrandbits", random_getrandbits as *const u8),
        ("getstate", random_getstate as *const u8),
        ("setstate", random_setstate as *const u8),
    ];
    for &(method_name, code) in methods {
        let interned = intern(method_name);
        // SAFETY: Live builtin entry points with the runtime calling convention.
        let function = unsafe { abi::pon_make_function(code, VARIADIC_ARITY, interned) };
        if !function.is_null() {
            // SAFETY: Freshly allocated namespace owned by this type.
            unsafe { (&mut *namespace).set(interned, function) };
        }
    }
    ty.tp_dict = namespace.cast::<PyObject>();
    let ty = Box::into_raw(Box::new(ty));
    crate::sync::register_namespaced_type(ty);
    crate::sync::type_modified(ty);
    ty as usize
});

fn random_type() -> *mut PyType {
    *RANDOM_TYPE as *mut PyType
}

// ---------------------------------------------------------------------------
// Methods (argv[0] is the bound receiver)

fn untag(object: *mut PyObject) -> *mut PyObject {
    crate::tag::untag_arg(object)
}

fn raise(kind: ExceptionKind, message: &str) -> *mut PyObject {
    crate::abi::exc::raise_kind_error_text(kind, message)
}

/// Splits the bound receiver off the argument window.
unsafe fn method_args(
    argv: *mut *mut PyObject,
    argc: usize,
    name: &str,
) -> Result<(*mut PyObject, Vec<*mut PyObject>), *mut PyObject> {
    if argv.is_null() || argc == 0 {
        return Err(raise(
            ExceptionKind::TypeError,
            &format!("Random.{name}() missing receiver"),
        ));
    }
    // SAFETY: The caller passes a live argv window of length argc.
    let raw = unsafe { core::slice::from_raw_parts(argv, argc) };
    let receiver = untag(raw[0]);
    Ok((receiver, raw[1..].iter().copied().map(untag).collect()))
}

/// Seeding core shared by `seed()` and `Random(...)` construction: NULL/None
/// seeds from OS entropy; ints seed from the absolute value's 32-bit digits
/// (little-endian); anything else seeds from `(size_t)PyObject_Hash(arg)`,
/// exactly like the C-level `random_seed`. The vendored Python layer
/// transforms str/bytes/bytearray to ints and rejects unsupported types
/// first, so floats are the main non-int visitors; unhashable objects
/// surface the hash TypeError. Returns None, or NULL with an exception set.
unsafe fn seed_receiver(receiver: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    let is_none = arg.is_null()
        // SAFETY: Type probe tolerates any live object.
        || unsafe { crate::types::dict::type_name(arg) } == Some("NoneType");
    let state = if is_none {
        Mt19937::from_entropy()
    } else {
        // SAFETY: Type probe tolerates any live object.
        let magnitude = match unsafe { to_bigint_including_bool(arg) } {
            Some(value) => value.abs(),
            // SAFETY: `hash_object` tolerates any live object.
            None => match unsafe { crate::types::dict::hash_object(arg) } {
                Ok(hash) => BigInt::from(hash as usize),
                Err(message) => return raise(ExceptionKind::TypeError, &message),
            },
        };
        let (_, digits) = magnitude.to_u32_digits();
        let key: &[u32] = if digits.is_empty() { &[0] } else { &digits };
        let mut state = Mt19937 { mt: [0; N], mti: N + 1 };
        state.init_by_array(key);
        state
    };
    let mut states = STATES.lock().unwrap_or_else(|poison| poison.into_inner());
    states.insert(receiver as usize, state);
    // SAFETY: Singleton accessor.
    unsafe { abi::pon_none() }
}

/// `seed(n=None)`, dispatching to the shared seeding core.
unsafe extern "C" fn random_seed(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Caller passes a live argv window.
    let (receiver, args) = match unsafe { method_args(argv, argc, "seed") } {
        Ok(pair) => pair,
        Err(error) => return error,
    };
    if args.len() > 1 {
        return raise(
            ExceptionKind::TypeError,
            &format!("seed() takes at most 1 argument ({} given)", args.len()),
        );
    }
    let arg = args.first().copied().unwrap_or(ptr::null_mut());
    // SAFETY: Receiver and argument are live objects.
    unsafe { seed_receiver(receiver, arg) }
}

/// `_random.Random(seed=None)`: construction seeds immediately (CPython's
/// `random_new`), so same-seed instances are deterministic. Heap subclasses
/// keep constructing through `type_new` + the Python `__init__` chain.
unsafe extern "C" fn random_new(cls: *mut PyType, args: *mut PyObject, kwargs: *mut PyObject) -> *mut PyObject {
    if !kwargs.is_null() {
        return raise(ExceptionKind::TypeError, "Random() takes no keyword arguments");
    }
    // SAFETY: `positional_args_from_object` tolerates any live carrier.
    let positional = match unsafe { crate::types::type_::positional_args_from_object(args) } {
        Ok(positional) => positional,
        Err(message) => return raise(ExceptionKind::TypeError, &message),
    };
    if positional.len() > 1 {
        return raise(
            ExceptionKind::TypeError,
            &format!("Random expected at most 1 argument, got {}", positional.len()),
        );
    }
    // SAFETY: Bare allocation of a non-exception heap-layout instance.
    let instance = unsafe { crate::types::type_::type_new(cls, ptr::null_mut(), ptr::null_mut()) };
    if instance.is_null() {
        return ptr::null_mut();
    }
    let arg = positional.first().copied().map_or(ptr::null_mut(), untag);
    // SAFETY: Freshly allocated receiver; the argument is a live object.
    if unsafe { seed_receiver(instance, arg) }.is_null() {
        return ptr::null_mut();
    }
    instance
}

unsafe extern "C" fn random_random(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    let (receiver, args) = match unsafe { method_args(argv, argc, "random") } {
        Ok(pair) => pair,
        Err(error) => return error,
    };
    if !args.is_empty() {
        return raise(ExceptionKind::TypeError, "random() takes no arguments");
    }
    crate::types::float::from_f64(with_state(receiver, Mt19937::random))
}

/// `getrandbits(k)`: k random bits as a non-negative int, assembled from
/// 32-bit words little-endian first (matches `_PyLong_FromByteArray` use).
unsafe extern "C" fn random_getrandbits(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    let (receiver, args) = match unsafe { method_args(argv, argc, "getrandbits") } {
        Ok(pair) => pair,
        Err(error) => return error,
    };
    if args.len() != 1 {
        return raise(
            ExceptionKind::TypeError,
            &format!("getrandbits() takes exactly 1 argument ({} given)", args.len()),
        );
    }
    // SAFETY: Type probe tolerates any live object.
    let Some(k) = (unsafe { to_bigint_including_bool(args[0]) }) else {
        return raise(
            ExceptionKind::TypeError,
            &format!(
                "'{}' object cannot be interpreted as an integer",
                // SAFETY: Same contract as above.
                unsafe { crate::types::dict::type_name(args[0]) }.unwrap_or("object")
            ),
        );
    };
    if k.is_negative() {
        return raise(ExceptionKind::ValueError, "number of bits must be non-negative");
    }
    if k.is_zero() {
        return from_bigint(BigInt::zero());
    }
    let Some(k) = k.to_u64() else {
        return raise(ExceptionKind::OverflowError, "number of bits is too large");
    };
    let words = ((k - 1) / 32 + 1) as usize;
    let digits = with_state(receiver, |state| {
        let mut digits = Vec::with_capacity(words);
        for index in 0..words {
            let mut r = state.genrand();
            if index == words - 1 && k % 32 != 0 {
                r >>= 32 - (k % 32) as u32;
            }
            digits.push(r);
        }
        digits
    });
    from_bigint(BigInt::new(Sign::Plus, digits))
}

/// `getstate()`: 625-tuple of the 624 state words plus the index.
unsafe extern "C" fn random_getstate(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    let (receiver, args) = match unsafe { method_args(argv, argc, "getstate") } {
        Ok(pair) => pair,
        Err(error) => return error,
    };
    if !args.is_empty() {
        return raise(ExceptionKind::TypeError, "getstate() takes no arguments");
    }
    let (mt, mti) = with_state(receiver, |state| (state.mt, state.mti));
    let mut items = Vec::with_capacity(N + 1);
    for word in mt {
        let boxed = crate::types::int::from_i64(i64::from(word));
        if boxed.is_null() {
            return ptr::null_mut();
        }
        items.push(boxed);
    }
    let index = crate::types::int::from_i64(mti as i64);
    if index.is_null() {
        return ptr::null_mut();
    }
    items.push(index);
    // SAFETY: `items` is a live window for the duration of the call.
    unsafe { abi::seq::pon_build_tuple(items.as_mut_ptr(), items.len()) }
}

/// `setstate(state)`: restores a 625-tuple produced by `getstate`.
unsafe extern "C" fn random_setstate(argv: *mut *mut PyObject, argc: usize) -> *mut PyObject {
    // SAFETY: Runtime builtin calling convention passes a live argv window.
    let (receiver, args) = match unsafe { method_args(argv, argc, "setstate") } {
        Ok(pair) => pair,
        Err(error) => return error,
    };
    if args.len() != 1 {
        return raise(
            ExceptionKind::TypeError,
            &format!("setstate() takes exactly 1 argument ({} given)", args.len()),
        );
    }
    let state_obj = args[0];
    // SAFETY: Type probe tolerates any live object.
    if unsafe { crate::types::dict::type_name(state_obj) } != Some("tuple") {
        return raise(ExceptionKind::TypeError, "state vector must be a tuple");
    }
    // SAFETY: Exact tuples carry the PyTuple layout probed above.
    let items = unsafe { (*state_obj.cast::<crate::types::tuple::PyTuple>()).as_slice().to_vec() };
    if items.len() != N + 1 {
        return raise(ExceptionKind::ValueError, "state vector is the wrong size");
    }
    let mut mt = [0u32; N];
    for (slot, &item) in mt.iter_mut().zip(&items) {
        // SAFETY: Tuple members are live objects.
        let Some(word) = (unsafe { to_bigint_including_bool(untag(item)) }) else {
            return raise(ExceptionKind::TypeError, "state vector items must be integers");
        };
        // CPython reads the words through PyLong_AsUnsignedLong casts.
        let Some(word) = word.to_u32().or_else(|| word.to_i64().map(|w| w as u32)) else {
            return raise(ExceptionKind::OverflowError, "state vector item out of range");
        };
        *slot = word;
    }
    // SAFETY: Same contract as above.
    let Some(mti) = (unsafe { to_bigint_including_bool(untag(items[N])) }).and_then(|value| value.to_i64()) else {
        return raise(ExceptionKind::TypeError, "state vector items must be integers");
    };
    if mti < 0 || mti > N as i64 + 1 {
        return raise(ExceptionKind::ValueError, "invalid state");
    }
    let mut states = STATES.lock().unwrap_or_else(|poison| poison.into_inner());
    states.insert(receiver as usize, Mt19937 { mt, mti: mti as usize });
    drop(states);
    // SAFETY: Singleton accessor.
    unsafe { abi::pon_none() }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn seeded(key: &[u32]) -> Mt19937 {
        let mut state = Mt19937 { mt: [0; N], mti: N + 1 };
        state.init_by_array(key);
        state
    }

    #[test]
    fn init_by_array_matches_mt19937ar_reference() {
        // Canonical mt19937ar.c demo key. Both the first-block outputs and
        // the post-reload window (indices 622..628, spanning the mti >= N
        // regeneration loops) were probed against CPython 3.14
        // random.Random(0x456<<96 | 0x345<<64 | 0x234<<32 | 0x123)
        // .getrandbits(32) during authoring.
        let mut mt = seeded(&[0x123, 0x234, 0x345, 0x456]);
        let outputs: Vec<u32> = (0..628).map(|_| mt.genrand()).collect();
        assert_eq!(outputs[..5], [1_067_595_299, 955_945_823, 477_289_528, 4_107_218_783, 4_228_976_476]);
        assert_eq!(
            outputs[622..628],
            [853_571_438, 144_400_272, 3_768_408_841, 782_634_401, 2_161_109_003, 570_039_522]
        );
    }

    #[test]
    fn random_matches_cpython_seed_42() {
        // CPython: random.Random(42).random() twice. Seeding 42 goes through
        // init_by_array([42]); the expected doubles pin the 27+26-bit
        // assembly in random() exactly.
        let mut mt = seeded(&[42]);
        assert_eq!(mt.random(), 0.639_426_798_457_883_7);
        assert_eq!(mt.random(), 0.025_010_755_222_666_936);
    }

    #[test]
    fn state_words_fully_determine_the_sequence() {
        // getstate/setstate contract: restoring (mt, mti) mid-stream must
        // replay the identical word sequence, including across a block
        // reload (700 draws > N).
        let mut mt = seeded(&[7]);
        for _ in 0..3 {
            mt.genrand();
        }
        let (saved_mt, saved_mti) = (mt.mt, mt.mti);
        let first: Vec<u32> = (0..700).map(|_| mt.genrand()).collect();
        mt.mt = saved_mt;
        mt.mti = saved_mti;
        let replay: Vec<u32> = (0..700).map(|_| mt.genrand()).collect();
        assert_eq!(first, replay);
    }
}
