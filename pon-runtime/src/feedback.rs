//! Phase-B/D feedback cells shared by tier-0 helpers and later tier-1 codegen.
//!
//! The runtime records only hints here.  A racy or stale read may choose a bad
//! speculation, but guards in generated code remain the source of truth, so the
//! worst outcome is a side exit rather than incorrect Python semantics.

use core::sync::atomic::{AtomicU32, Ordering};

const TAG_MASK: u32 = 0xff;
const RHS_SHIFT: u32 = 8;
const COUNT_SHIFT: u32 = 16;
const STATE_SHIFT: u32 = 24;
const COUNT_MAX: u8 = u8::MAX;

const STATE_EMPTY: u8 = 0;
const STATE_MONO: u8 = 1;
const STATE_POLY: u8 = 2;
const STATE_MEGA: u8 = 3;

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
/// The packed word is updated atomically as a unit:
///
/// - byte 0: left/unary [`TypeTag`]
/// - byte 1: right [`TypeTag`] (`Other` for unary observations)
/// - byte 2: saturating observation count
/// - byte 3: state (`empty`, monomorphic, polymorphic, megamorphic)
///
/// Recording is benign-racy: a contended compare-exchange may be dropped.  This
/// deliberately avoids locking, allocation, unwinding, or dependence on native
/// exception machinery from helper hot paths.
#[repr(C)]
#[derive(Debug)]
pub struct FeedbackCell {
    /// Packed feedback state.  Consumers must go through [`record`](Self::record)
    /// and [`speculate`](Self::speculate) rather than depending on byte details.
    pub packed: AtomicU32,
}

impl FeedbackCell {
    /// Empty feedback cell.
    pub const EMPTY: Self = Self {
        packed: AtomicU32::new(0),
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
}

impl Default for FeedbackCell {
    fn default() -> Self {
        Self::EMPTY
    }
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
}
