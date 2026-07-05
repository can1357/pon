use std::{fmt, path::PathBuf};

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug)]
pub enum Error {
	Io(std::io::Error),
	InvalidName(String),
	InvalidRequirement(String),
	InvalidMarker(String),
	InvalidWheelFilename(String),
	InvalidSpecifier(String),
	UnsupportedArtifact(String),
	Manifest { path: PathBuf, message: String },
	Index(String),
	Cli(String),
}

impl Error {
	pub fn manifest(path: impl Into<PathBuf>, message: impl Into<String>) -> Self {
		Self::Manifest { path: path.into(), message: message.into() }
	}
}

impl fmt::Display for Error {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Io(error) => write!(f, "{error}"),
			Self::InvalidName(name) => write!(f, "invalid package name `{name}`"),
			Self::InvalidRequirement(req) => write!(f, "invalid requirement `{req}`"),
			Self::InvalidMarker(marker) => write!(f, "invalid environment marker `{marker}`"),
			Self::InvalidWheelFilename(name) => write!(f, "invalid wheel filename `{name}`"),
			Self::InvalidSpecifier(spec) => write!(f, "invalid version specifier `{spec}`"),
			Self::UnsupportedArtifact(message) => f.write_str(message),
			Self::Manifest { path, message } => write!(f, "{}: {message}", path.display()),
			Self::Index(message) | Self::Cli(message) => f.write_str(message),
		}
	}
}

impl std::error::Error for Error {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			Self::Io(error) => Some(error),
			Self::InvalidName(_)
			| Self::InvalidRequirement(_)
			| Self::InvalidMarker(_)
			| Self::InvalidWheelFilename(_)
			| Self::InvalidSpecifier(_)
			| Self::UnsupportedArtifact(_)
			| Self::Manifest { .. }
			| Self::Index(_)
			| Self::Cli(_) => None,
		}
	}
}

impl From<std::io::Error> for Error {
	fn from(error: std::io::Error) -> Self {
		Self::Io(error)
	}
}
