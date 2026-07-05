//! PubGrub package identity types for pon dependency resolution.

use std::fmt;

/// Package node solved by PubGrub for root requirements, distributions, and
/// extras.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum PonPackage {
	/// Synthetic root package that owns user and project requirements.
	Root,
	/// A normalized distribution package.
	Dist(String),
	/// A normalized distribution extra modeled as a separate package.
	Extra(String, String),
}

impl PonPackage {
	/// Return whether this package is the synthetic resolver root.
	pub fn is_root(&self) -> bool {
		matches!(self, Self::Root)
	}

	/// Return the normalized distribution name for distribution and extra nodes.
	pub fn dist_name(&self) -> Option<&str> {
		match self {
			Self::Root => None,
			Self::Dist(name) | Self::Extra(name, _) => Some(name.as_str()),
		}
	}
}

impl fmt::Display for PonPackage {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Root => f.write_str("root"),
			Self::Dist(name) => f.write_str(name),
			Self::Extra(name, extra) => write!(f, "{name}[{extra}]"),
		}
	}
}
