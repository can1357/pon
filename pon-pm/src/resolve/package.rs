use std::fmt;

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum PonPackage {
    Root,
    Dist(String),
    Extra(String, String),
}

impl PonPackage {
    pub fn is_root(&self) -> bool {
        matches!(self, Self::Root)
    }

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
