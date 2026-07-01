//! Phase-B/D feedback cells shared by tier-0 helpers and later tier-1 codegen.
//!
//! The runtime records only hints here.  A racy or stale read may choose a bad
//! speculation, but guards in generated code remain the source of truth, so the
//! worst outcome is a side exit rather than incorrect Python semantics.
//!
//! # J0.3 inline-cache substrate
//!
//! Beyond arithmetic operand profiling, each cell can hold one inline-cache
//! record: an attribute load ([`AttrIC`]), a global/builtin load
//! ([`GlobalIC`]), or a call site ([`CallIC`]).  Which interpretation applies
//! is decided STATICALLY: lowering reserves one feedback slot per
//! specializable operation, and the operation fixes the cell kind
//! ([`FeedbackKind`]) — no kind bits are stored in the cell at runtime.
//!
//! Layout and concurrency contracts are pinned in
//! `plans/pon-pin-J03-inline-caches.md`.  Tier-1 codegen emits raw loads
//! against the `FEEDBACK_CELL_*_OFFSET` constants below; the cell size and
//! field offsets are frozen by compile-time assertions.

use core::mem::{align_of, offset_of, size_of};
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};

const TAG_MASK: u32 = 0xff;
const RHS_SHIFT: u32 = 8;
const COUNT_SHIFT: u32 = 16;
const STATE_SHIFT: u32 = 24;
const COUNT_MAX: u8 = u8::MAX;

const STATE_EMPTY: u8 = 0;
const STATE_MONO: u8 = 1;
const STATE_POLY: u8 = 2;
const STATE_MEGA: u8 = 3;

/// `packed` value while an IC recorder is mid-publication; guards miss on it.
pub const IC_WRITING: u32 = u32::MAX;
/// `packed` value of a published [`CallIC`] record (call cells carry no version).
pub const IC_CALL_PRESENT: u32 = 1;

const ATTR_KIND_SHIFT: u64 = 32;

/// Compact operand-type tag recorded by tier-0 helpers.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeTag {
    /// Unknown, unsupported, or intentionally unspecialized shape.
    Other = 0,
    /// Exact Python `int` object when compactness is not known.
    Int = 1,
    /// Exact compact `int` whose payload fits the inline `i64` fast path.
    IntI64 = 2,
    /// Exact Python `float` object.
    Float = 3,
    /// Exact Python `bool` object; kept distinct from `int` for correctness.
    Bool = 4,
    /// Exact Python `str` object.
    Str = 5,
}

impl TypeTag {
    fn from_byte(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Other),
            1 => Some(Self::Int),
            2 => Some(Self::IntI64),
            3 => Some(Self::Float),
            4 => Some(Self::Bool),
            5 => Some(Self::Str),
            _ => None,
        }
    }
}

/// One profiling cell for a specializable operation.
///
/// ## Word layout (LOCKED — tier-1 emits raw loads at these offsets)
///
/// | offset | field      | arith       | [`AttrIC`]              | [`GlobalIC`]         | [`CallIC`]        |
/// |--------|------------|-------------|-------------------------|----------------------|-------------------|
/// | 0      | `packed`   | packed tags | `type_version`          | `dict_version`       | presence marker   |
/// | 4      | `aux`      | 0           | reserved (0)            | `builtins_version`   | reserved (0)      |
/// | 8      | `payload`  | 0           | `kind << 32 \| offset`  | `value_ptr`          | reserved (0)      |
/// | 16     | `identity` | 0           | type address            | globals-dict address | `callee_identity` |
/// | 24     | `extra`    | 0           | descriptor address      | reserved (0)         | reserved (0)      |
///
/// For arithmetic cells the packed word is updated atomically as a unit:
///
/// - byte 0: left/unary [`TypeTag`]
/// - byte 1: right [`TypeTag`] (`Other` for unary observations)
/// - byte 2: saturating observation count
/// - byte 3: state (`empty`, monomorphic, polymorphic, megamorphic)
///
/// For IC cells `packed` doubles as the seqlock word: `0` = empty,
/// [`IC_WRITING`] = a recorder is mid-publication, any other value = the
/// guard version of a published record.  Readers load `packed` (Acquire),
/// then the payload words, then re-check `packed` — the version is checked
/// AFTER the payload load, and any mismatch means the read may be torn and
/// takes the slow path.  Recorders claim the cell by CAS-ing `packed` to
/// [`IC_WRITING`], store the payload words, then publish the guard version
/// with a Release store.  A failed claim drops the observation.
///
/// Recording is benign-racy: a contended compare-exchange may be dropped.  This
/// deliberately avoids locking, allocation, unwinding, or dependence on native
/// exception machinery from helper hot paths.
#[repr(C)]
#[derive(Debug)]
pub struct FeedbackCell {
    /// Guard/seqlock word.  Consumers must go through the record/consult API
    /// rather than depending on byte details.
    pub packed: AtomicU32,
    /// Secondary guard word ([`GlobalIC`] builtins version); reserved 0 otherwise.
    pub aux: AtomicU32,
    /// Kind-specific payload; see the layout table.
    pub payload: AtomicU64,
    /// Guarded identity (type / globals-dict / callee address); 0 latches megamorphic.
    pub identity: AtomicU64,
    /// Overflow payload ([`AttrIC`] descriptor address); reserved 0 otherwise.
    pub extra: AtomicU64,
}

/// Total size of one [`FeedbackCell`] in bytes (power-of-two stride).
pub const FEEDBACK_CELL_SIZE: usize = 32;
/// Byte offset of [`FeedbackCell::packed`] inside a cell.
pub const FEEDBACK_CELL_PACKED_OFFSET: usize = 0;
/// Byte offset of [`FeedbackCell::aux`] inside a cell.
pub const FEEDBACK_CELL_AUX_OFFSET: usize = 4;
/// Byte offset of [`FeedbackCell::payload`] inside a cell.
pub const FEEDBACK_CELL_PAYLOAD_OFFSET: usize = 8;
/// Byte offset of [`FeedbackCell::identity`] inside a cell.
pub const FEEDBACK_CELL_IDENTITY_OFFSET: usize = 16;
/// Byte offset of [`FeedbackCell::extra`] inside a cell.
pub const FEEDBACK_CELL_EXTRA_OFFSET: usize = 24;

const _: () = {
    assert!(size_of::<FeedbackCell>() == FEEDBACK_CELL_SIZE);
    assert!(align_of::<FeedbackCell>() == 8);
    assert!(offset_of!(FeedbackCell, packed) == FEEDBACK_CELL_PACKED_OFFSET);
    assert!(offset_of!(FeedbackCell, aux) == FEEDBACK_CELL_AUX_OFFSET);
    assert!(offset_of!(FeedbackCell, payload) == FEEDBACK_CELL_PAYLOAD_OFFSET);
    assert!(offset_of!(FeedbackCell, identity) == FEEDBACK_CELL_IDENTITY_OFFSET);
    assert!(offset_of!(FeedbackCell, extra) == FEEDBACK_CELL_EXTRA_OFFSET);
};

impl FeedbackCell {
    /// Empty feedback cell: every word zero.
    pub const EMPTY: Self = Self {
        packed: AtomicU32::new(0),
        aux: AtomicU32::new(0),
        payload: AtomicU64::new(0),
        identity: AtomicU64::new(0),
        extra: AtomicU64::new(0),
    };

    /// Records one observed operand shape.
    ///
    /// This method is no-throw and non-blocking.  On contention it may drop the
    /// observation; subsequent executions can record another sample.
    pub fn record(&self, lhs: TypeTag, rhs: TypeTag) {
        let current = self.packed.load(Ordering::Relaxed);
        let next = next_state(current, lhs, rhs);
        if next != current {
            let _ = self
                .packed
                .compare_exchange(current, next, Ordering::Relaxed, Ordering::Relaxed);
        }
    }

    /// Returns the monomorphic shape that tier-1 code may speculate on.
    ///
    /// `None` means the cell is empty, polymorphic, megamorphic, or contains a
    /// future tag unknown to this runtime build.
    #[must_use]
    pub fn speculate(&self) -> Option<(TypeTag, TypeTag)> {
        let packed = self.packed.load(Ordering::Relaxed);
        if state_of(packed) != STATE_MONO {
            return None;
        }
        let lhs = TypeTag::from_byte(lhs_of(packed))?;
        let rhs = TypeTag::from_byte(rhs_of(packed))?;
        Some((lhs, rhs))
    }

    /// Consults an attribute IC: `Some` only when the recorded identity and
    /// version both match the receiver's live type state.
    ///
    /// `type_identity` is the receiver's type-object address and
    /// `live_type_version` its current `PyType::version()`.  The live-version
    /// compare makes staleness structurally impossible: a record captured
    /// before any subsequent `bump_version` can never match again.
    #[must_use]
    pub fn attr_hit(&self, type_identity: usize, live_type_version: u32) -> Option<AttrIC> {
        let words = self.ic_snapshot()?;
        if words.identity != type_identity as u64 || words.version != live_type_version {
            return None;
        }
        let kind = AttrCacheKind::from_byte((words.payload >> ATTR_KIND_SHIFT) as u8)?;
        Some(AttrIC {
            type_version: words.version,
            kind,
            offset: words.payload as u32,
            descriptor: words.extra as usize,
        })
    }

    /// Publishes an attribute IC record for the receiver type.
    ///
    /// `ic.type_version` must be the tag captured BEFORE the slow lookup that
    /// produced the record: if the type mutated during the lookup, the stale
    /// version simply never matches and the next execution re-records.
    /// Records carrying the invalid sentinel (`0`) or [`IC_WRITING`] are
    /// silently refused, as is a NULL `type_identity`.
    pub fn record_attr(&self, type_identity: usize, ic: AttrIC) {
        if type_identity == 0 || ic.type_version == 0 || ic.type_version == IC_WRITING {
            return;
        }
        let payload = u64::from(ic.offset) | (u64::from(ic.kind as u8) << ATTR_KIND_SHIFT);
        self.ic_publish(ic.type_version, 0, payload, type_identity as u64, ic.descriptor as u64);
    }

    /// Returns the published attribute record and its guarded type identity
    /// without live-version validation (tier-1 compile-time snapshot).
    #[must_use]
    pub fn attr_snapshot(&self) -> Option<(usize, AttrIC)> {
        let words = self.ic_snapshot()?;
        let kind = AttrCacheKind::from_byte((words.payload >> ATTR_KIND_SHIFT) as u8)?;
        Some((
            words.identity as usize,
            AttrIC {
                type_version: words.version,
                kind,
                offset: words.payload as u32,
                descriptor: words.extra as usize,
            },
        ))
    }

    /// Consults a global-load IC: `Some` only when the recorded globals-dict
    /// identity and version match, and — for records resolved through the
    /// builtins dict — the builtins version matches too.
    ///
    /// A recorded `builtins_version` of `0` means the name was bound directly
    /// in the globals dict, so builtins mutations are irrelevant to it.
    #[must_use]
    pub fn global_hit(
        &self,
        dict_identity: usize,
        live_dict_version: u32,
        live_builtins_version: u32,
    ) -> Option<GlobalIC> {
        let words = self.ic_snapshot()?;
        if words.identity != dict_identity as u64 || words.version != live_dict_version {
            return None;
        }
        if words.aux != 0 && words.aux != live_builtins_version {
            return None;
        }
        Some(GlobalIC {
            dict_version: words.version,
            builtins_version: words.aux,
            value_ptr: words.payload as usize,
        })
    }

    /// Publishes a global-load IC record guarded by the globals dict identity.
    ///
    /// Versions must be captured BEFORE the slow lookup that produced
    /// `value_ptr`.  Records with an invalid (`0`) or reserved
    /// ([`IC_WRITING`]) `dict_version`, an [`IC_WRITING`] `builtins_version`,
    /// or a NULL `dict_identity` are silently refused.
    pub fn record_global(&self, dict_identity: usize, ic: GlobalIC) {
        if dict_identity == 0
            || ic.dict_version == 0
            || ic.dict_version == IC_WRITING
            || ic.builtins_version == IC_WRITING
        {
            return;
        }
        self.ic_publish(ic.dict_version, ic.builtins_version, ic.value_ptr as u64, dict_identity as u64, 0);
    }

    /// Returns the published global record and its guarded dict identity
    /// without live-version validation (tier-1 compile-time snapshot).
    #[must_use]
    pub fn global_snapshot(&self) -> Option<(usize, GlobalIC)> {
        let words = self.ic_snapshot()?;
        Some((
            words.identity as usize,
            GlobalIC {
                dict_version: words.version,
                builtins_version: words.aux,
                value_ptr: words.payload as usize,
            },
        ))
    }

    /// Consults a call IC: `Some` only when this site has been observed
    /// monomorphic on exactly `callee_identity`.
    #[must_use]
    pub fn call_hit(&self, callee_identity: usize) -> Option<CallIC> {
        let words = self.ic_snapshot()?;
        if words.identity == 0 || words.identity != callee_identity as u64 {
            return None;
        }
        Some(CallIC { callee_identity })
    }

    /// Records one observed call target.
    ///
    /// First target wins; observing a second, different target latches the
    /// cell megamorphic (identity `0`), after which [`call_hit`](Self::call_hit)
    /// and [`call_snapshot`](Self::call_snapshot) permanently miss.
    pub fn record_call(&self, ic: CallIC) {
        if ic.callee_identity == 0 {
            return;
        }
        match self.ic_snapshot() {
            None => self.ic_publish(IC_CALL_PRESENT, 0, 0, ic.callee_identity as u64, 0),
            Some(words) if words.identity == 0 || words.identity == ic.callee_identity as u64 => {}
            Some(_) => self.ic_publish(IC_CALL_PRESENT, 0, 0, 0, 0),
        }
    }

    /// Returns the monomorphic call target observed so far (tier-1 snapshot).
    #[must_use]
    pub fn call_snapshot(&self) -> Option<CallIC> {
        let words = self.ic_snapshot()?;
        if words.identity == 0 {
            return None;
        }
        Some(CallIC {
            callee_identity: words.identity as usize,
        })
    }

    /// Seqlock read of the IC words; `None` when empty, mid-write, or torn.
    ///
    /// This is the crossbeam-style validated reader: Acquire first load,
    /// Relaxed payload loads, Acquire fence, Relaxed re-check.  See the pin
    /// doc for the torn-read proof.
    fn ic_snapshot(&self) -> Option<IcWords> {
        let v1 = self.packed.load(Ordering::Acquire);
        if v1 == 0 || v1 == IC_WRITING {
            return None;
        }
        let aux = self.aux.load(Ordering::Relaxed);
        let payload = self.payload.load(Ordering::Relaxed);
        let identity = self.identity.load(Ordering::Relaxed);
        let extra = self.extra.load(Ordering::Relaxed);
        fence(Ordering::Acquire);
        if self.packed.load(Ordering::Relaxed) != v1 {
            return None;
        }
        Some(IcWords {
            version: v1,
            aux,
            payload,
            identity,
            extra,
        })
    }

    /// Seqlock publication: claim via CAS to [`IC_WRITING`], store payload
    /// words, publish `version` with Release.  Contended claims are dropped,
    /// mirroring the arithmetic cell's compare-exchange policy.
    fn ic_publish(&self, version: u32, aux: u32, payload: u64, identity: u64, extra: u64) {
        debug_assert!(version != 0 && version != IC_WRITING);
        let observed = self.packed.load(Ordering::Relaxed);
        if observed == IC_WRITING {
            return;
        }
        if self
            .packed
            .compare_exchange(observed, IC_WRITING, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        fence(Ordering::Release);
        self.aux.store(aux, Ordering::Relaxed);
        self.payload.store(payload, Ordering::Relaxed);
        self.identity.store(identity, Ordering::Relaxed);
        self.extra.store(extra, Ordering::Relaxed);
        self.packed.store(version, Ordering::Release);
    }
}

impl Default for FeedbackCell {
    fn default() -> Self {
        Self::EMPTY
    }
}

/// Consistent multi-word snapshot produced by the seqlock reader.
#[derive(Clone, Copy)]
struct IcWords {
    version: u32,
    aux: u32,
    payload: u64,
    identity: u64,
    extra: u64,
}

/// Statically-assigned cell kind.
///
/// Lowering reserves one feedback slot per specializable operation, so the
/// operation kind — not runtime bits — decides how a cell's words are
/// interpreted.  Mixing kinds on one slot is a lowering bug.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FeedbackKind {
    /// Arithmetic/compare operand-shape profiling (packed word only).
    Arith,
    /// Attribute-load inline cache ([`AttrIC`]).
    Attr,
    /// Global/builtin-load inline cache ([`GlobalIC`]).
    Global,
    /// Call-target inline cache ([`CallIC`]).
    Call,
}

/// Where a cached attribute lives; stored in bits 32..40 of the payload word.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttrCacheKind {
    /// Fixed-offset storage inside the instance (`__slots__` storage);
    /// `offset` is the byte offset from the instance base.
    Slot = 1,
    /// Attribute lives in the instance dict; `offset` is the split-key layout
    /// index hint (N4) or `0` for a plain probe skip.
    DictOffset = 2,
    /// MRO lookup produced a descriptor; `extra` caches its address.
    Descriptor = 3,
}

impl AttrCacheKind {
    fn from_byte(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Slot),
            2 => Some(Self::DictOffset),
            3 => Some(Self::Descriptor),
            _ => None,
        }
    }
}

/// One attribute inline-cache record (J0.3 pin: `AttrIC { type_version, kind, offset }`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AttrIC {
    /// Receiver-type version tag captured BEFORE the slow lookup.
    pub type_version: u32,
    /// Where the attribute lives.
    pub kind: AttrCacheKind,
    /// Kind-specific offset; see [`AttrCacheKind`].
    pub offset: u32,
    /// Address of the cached descriptor object ([`AttrCacheKind::Descriptor`]), else 0.
    pub descriptor: usize,
}

/// One global-load inline-cache record (J0.3 pin: `GlobalIC { dict_version, value_ptr }`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GlobalIC {
    /// Globals-dict version captured BEFORE the slow lookup.
    pub dict_version: u32,
    /// Builtins-dict version, or `0` when the name was bound in globals and
    /// the builtins guard is skipped.
    pub builtins_version: u32,
    /// Address of the cached value object.
    pub value_ptr: usize,
}

/// One call-site inline-cache record (J0.3 pin: `CallIC { callee_identity }`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CallIC {
    /// Address of the observed callee object; never 0 in a published record.
    pub callee_identity: usize,
}

/// Per-function feedback vector: one cell per lowering-assigned feedback slot.
#[repr(C)]
#[derive(Debug)]
pub struct FeedbackVec(pub Box<[FeedbackCell]>);

impl FeedbackVec {
    /// Allocates `len` empty feedback cells.
    #[must_use]
    pub fn new(len: usize) -> Self {
        let cells = (0..len).map(|_| FeedbackCell::default()).collect::<Vec<_>>();
        Self(cells.into_boxed_slice())
    }

    /// Returns the number of cells in this vector.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns true when no feedback slots are reserved.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns a cell by feedback-slot index.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&FeedbackCell> {
        self.0.get(index)
    }
}

fn next_state(current: u32, lhs: TypeTag, rhs: TypeTag) -> u32 {
    let state = state_of(current);
    let count = count_of(current);
    match state {
        STATE_EMPTY => pack(lhs, rhs, 1, STATE_MONO),
        STATE_MONO => {
            if lhs_of(current) == lhs as u8 && rhs_of(current) == rhs as u8 {
                pack(lhs, rhs, count.saturating_add(1), STATE_MONO)
            } else {
                pack(lhs, rhs, count.saturating_add(1), STATE_POLY)
            }
        }
        STATE_POLY => {
            if lhs_of(current) == lhs as u8 && rhs_of(current) == rhs as u8 {
                pack(lhs, rhs, count.saturating_add(1), STATE_POLY)
            } else {
                pack(lhs, rhs, COUNT_MAX, STATE_MEGA)
            }
        }
        STATE_MEGA => current,
        _ => pack(lhs, rhs, 1, STATE_MONO),
    }
}

fn pack(lhs: TypeTag, rhs: TypeTag, count: u8, state: u8) -> u32 {
    (lhs as u32)
        | ((rhs as u32) << RHS_SHIFT)
        | ((count as u32) << COUNT_SHIFT)
        | ((state as u32) << STATE_SHIFT)
}

fn lhs_of(packed: u32) -> u8 {
    (packed & TAG_MASK) as u8
}

fn rhs_of(packed: u32) -> u8 {
    ((packed >> RHS_SHIFT) & TAG_MASK) as u8
}

fn count_of(packed: u32) -> u8 {
    ((packed >> COUNT_SHIFT) & TAG_MASK) as u8
}

fn state_of(packed: u32) -> u8 {
    ((packed >> STATE_SHIFT) & TAG_MASK) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_installs_monomorphic_speculation() {
        let cell = FeedbackCell::default();
        assert_eq!(cell.speculate(), None);

        cell.record(TypeTag::IntI64, TypeTag::IntI64);

        assert_eq!(cell.speculate(), Some((TypeTag::IntI64, TypeTag::IntI64)));
    }

    #[test]
    fn distinct_shape_clears_speculation_without_throwing() {
        let cell = FeedbackCell::default();
        cell.record(TypeTag::IntI64, TypeTag::IntI64);
        cell.record(TypeTag::Str, TypeTag::Str);

        assert_eq!(cell.speculate(), None);
    }

    #[test]
    fn feedback_vec_allocates_empty_cells() {
        let vec = FeedbackVec::new(2);

        assert_eq!(vec.len(), 2);
        assert_eq!(vec.get(0).and_then(FeedbackCell::speculate), None);
        assert_eq!(vec.get(1).and_then(FeedbackCell::speculate), None);
    }

    #[test]
    fn feedback_cell_layout_is_locked() {
        assert_eq!(size_of::<FeedbackCell>(), FEEDBACK_CELL_SIZE);
        assert_eq!(offset_of!(FeedbackCell, packed), FEEDBACK_CELL_PACKED_OFFSET);
        assert_eq!(offset_of!(FeedbackCell, aux), FEEDBACK_CELL_AUX_OFFSET);
        assert_eq!(offset_of!(FeedbackCell, payload), FEEDBACK_CELL_PAYLOAD_OFFSET);
        assert_eq!(offset_of!(FeedbackCell, identity), FEEDBACK_CELL_IDENTITY_OFFSET);
        assert_eq!(offset_of!(FeedbackCell, extra), FEEDBACK_CELL_EXTRA_OFFSET);
    }

    #[test]
    fn attr_ic_round_trips_and_guards_on_version_and_identity() {
        let cell = FeedbackCell::default();
        let ty = 0x1000usize;
        assert_eq!(cell.attr_hit(ty, 7), None);

        let ic = AttrIC {
            type_version: 7,
            kind: AttrCacheKind::Slot,
            offset: 24,
            descriptor: 0,
        };
        cell.record_attr(ty, ic);

        assert_eq!(cell.attr_hit(ty, 7), Some(ic));
        assert_eq!(cell.attr_hit(ty, 8), None, "type mutation must miss");
        assert_eq!(cell.attr_hit(0x2000, 7), None, "other receiver type must miss");
        assert_eq!(cell.attr_snapshot(), Some((ty, ic)));
    }

    #[test]
    fn attr_ic_descriptor_kind_carries_descriptor_address() {
        let cell = FeedbackCell::default();
        let ic = AttrIC {
            type_version: 3,
            kind: AttrCacheKind::Descriptor,
            offset: 0,
            descriptor: 0xdead_b0b0,
        };
        cell.record_attr(0x1000, ic);

        assert_eq!(cell.attr_hit(0x1000, 3), Some(ic));
    }

    #[test]
    fn attr_ic_refuses_sentinel_versions_and_null_identity() {
        let cell = FeedbackCell::default();
        let ic = AttrIC {
            type_version: 0,
            kind: AttrCacheKind::Slot,
            offset: 8,
            descriptor: 0,
        };
        cell.record_attr(0x1000, ic);
        assert_eq!(cell.attr_snapshot(), None, "invalid sentinel version never records");

        cell.record_attr(0x1000, AttrIC { type_version: IC_WRITING, ..ic });
        assert_eq!(cell.attr_snapshot(), None, "reserved writing marker never records");

        cell.record_attr(0, AttrIC { type_version: 5, ..ic });
        assert_eq!(cell.attr_snapshot(), None, "NULL type identity never records");
    }

    #[test]
    fn global_ic_guards_globals_and_builtins_versions() {
        let cell = FeedbackCell::default();
        let dict = 0x3000usize;
        let ic = GlobalIC {
            dict_version: 5,
            builtins_version: 9,
            value_ptr: 0xabc,
        };
        cell.record_global(dict, ic);

        assert_eq!(cell.global_hit(dict, 5, 9), Some(ic));
        assert_eq!(cell.global_hit(dict, 6, 9), None, "globals mutation must miss");
        assert_eq!(cell.global_hit(dict, 5, 10), None, "builtins mutation must miss");
        assert_eq!(cell.global_hit(0x4000, 5, 9), None, "other globals dict must miss");

        let globals_bound = GlobalIC {
            dict_version: 6,
            builtins_version: 0,
            value_ptr: 0xdef,
        };
        cell.record_global(dict, globals_bound);
        assert_eq!(
            cell.global_hit(dict, 6, 1234),
            Some(globals_bound),
            "builtins guard is skipped for globals-bound names"
        );
    }

    #[test]
    fn call_ic_is_first_target_wins_then_megamorphic() {
        let cell = FeedbackCell::default();
        assert_eq!(cell.call_hit(0x10), None);

        cell.record_call(CallIC { callee_identity: 0x10 });
        assert_eq!(cell.call_hit(0x10), Some(CallIC { callee_identity: 0x10 }));
        assert_eq!(cell.call_hit(0x20), None);
        assert_eq!(cell.call_snapshot(), Some(CallIC { callee_identity: 0x10 }));

        cell.record_call(CallIC { callee_identity: 0x20 });
        assert_eq!(cell.call_hit(0x10), None, "megamorphic latch must miss");
        assert_eq!(cell.call_hit(0x20), None, "megamorphic latch must miss");
        assert_eq!(cell.call_snapshot(), None);

        cell.record_call(CallIC { callee_identity: 0x10 });
        assert_eq!(cell.call_hit(0x10), None, "megamorphic latch is permanent");
    }

    #[test]
    fn ic_words_start_empty_and_arith_api_is_undisturbed() {
        let cell = FeedbackCell::default();
        assert_eq!(cell.attr_snapshot(), None);
        assert_eq!(cell.global_snapshot(), None);
        assert_eq!(cell.call_snapshot(), None);

        cell.record(TypeTag::IntI64, TypeTag::IntI64);
        assert_eq!(cell.speculate(), Some((TypeTag::IntI64, TypeTag::IntI64)));
    }
}
