//! Type lattice used by the future optimizing tier.

/// Speculative/static type lattice attached to IR SSA values and operation
/// sites.
///
/// The ordering is by precision: [`Type::Bottom`] is the unobserved identity
/// element for merges, concrete exact types sit in the middle, and
/// [`Type::Object`] is the boxed top type used when no specialization is sound.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum Type {
	/// Unobserved or unreachable value; identity for [`Type::join`].
	Bottom,
	/// Exact Python `int`/`PyLong` of any magnitude, including bignums.
	Int,
	/// Refinement of [`Type::Int`] that fits losslessly in an `i64`.
	IntI64,
	/// Exact Python `float`/`PyFloat`.
	Float,
	/// Exact Python `bool`; intentionally not a subtype of [`Type::Int`].
	Bool,
	/// Exact Python `str`/`PyUnicode`.
	Str,
	/// Exact concrete class identified by the runtime's interned type id.
	ExactClass(u32),
	/// Any boxed `*mut PyObject`; top of the lattice.
	Object,
}

impl Type {
	/// Least-upper-bound for control-flow merges.
	///
	/// [`Type::Bottom`] is the identity. Equal types are stable. The only
	/// primitive refinement relation is `IntI64 <= Int`, so `IntI64 join Int`
	/// yields [`Type::Int`]. All other differing non-bottom types widen to
	/// [`Type::Object`].
	#[must_use]
	pub fn join(self, other: Self) -> Self {
		match (self, other) {
			(Self::Bottom, ty) | (ty, Self::Bottom) => ty,
			(Self::Object, _) | (_, Self::Object) => Self::Object,
			(Self::IntI64, Self::Int) | (Self::Int, Self::IntI64) => Self::Int,
			(lhs, rhs) if lhs == rhs => lhs,
			_ => Self::Object,
		}
	}

	/// Return true when this type can be represented unboxed in a CLIF register.
	///
	/// Arbitrary-precision [`Type::Int`] is deliberately boxed; only the
	/// `i64` refinement and `f64` floats are unboxable in Phase D's typed tier.
	#[must_use]
	pub const fn is_unboxable(self) -> bool {
		matches!(self, Self::IntI64 | Self::Float)
	}

	/// Return the CLIF machine type for this type's unboxed representation.
	#[must_use]
	pub const fn clif_repr(self) -> Option<cranelift_codegen::ir::Type> {
		match self {
			Self::IntI64 => Some(cranelift_codegen::ir::types::I64),
			Self::Float => Some(cranelift_codegen::ir::types::F64),
			_ => None,
		}
	}
}

impl Default for Type {
	fn default() -> Self {
		Self::Bottom
	}
}
