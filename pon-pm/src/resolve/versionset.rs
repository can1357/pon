use std::cmp::Ordering;

use crate::error::{Error, Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Version {
    raw: String,
    release: Vec<u64>,
}

impl Version {
    pub fn parse(raw: impl AsRef<str>) -> Result<Self> {
        let raw = raw.as_ref().trim();
        if raw.is_empty() {
            return Err(Error::InvalidSpecifier(raw.to_owned()));
        }
        let release = raw
            .split('.')
            .map(|part| part.parse::<u64>().map_err(|_| Error::InvalidSpecifier(raw.to_owned())))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            raw: raw.to_owned(),
            release,
        })
    }

    #[must_use]
    pub fn raw(&self) -> &str {
        &self.raw
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let max = self.release.len().max(other.release.len());
        for index in 0..max {
            let left = self.release.get(index).copied().unwrap_or(0);
            let right = other.release.get(index).copied().unwrap_or(0);
            match left.cmp(&right) {
                Ordering::Equal => {}
                ordering => return ordering,
            }
        }
        Ordering::Equal
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operator {
    Eq,
    NotEq,
    Gt,
    Gte,
    Lt,
    Lte,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Specifier {
    pub operator: Operator,
    pub version: Version,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VersionSet {
    specifiers: Vec<Specifier>,
}

impl VersionSet {
    pub fn parse(raw: impl AsRef<str>) -> Result<Self> {
        let raw = raw.as_ref().trim();
        if raw.is_empty() || raw == "*" {
            return Ok(Self::default());
        }

        let mut specifiers = Vec::new();
        for part in raw.split(',') {
            let part = part.trim();
            let (operator, version) = parse_specifier(part)?;
            specifiers.push(Specifier { operator, version });
        }
        Ok(Self { specifiers })
    }

    #[must_use]
    pub fn contains(&self, version: &Version) -> bool {
        self.specifiers.iter().all(|specifier| {
            let ordering = version.cmp(&specifier.version);
            match specifier.operator {
                Operator::Eq => ordering.is_eq(),
                Operator::NotEq => !ordering.is_eq(),
                Operator::Gt => ordering.is_gt(),
                Operator::Gte => ordering.is_ge(),
                Operator::Lt => ordering.is_lt(),
                Operator::Lte => ordering.is_le(),
            }
        })
    }

    #[must_use]
    pub fn is_unbounded(&self) -> bool {
        self.specifiers.is_empty()
    }
}

fn parse_specifier(part: &str) -> Result<(Operator, Version)> {
    for (raw_op, operator) in [
        ("==", Operator::Eq),
        ("!=", Operator::NotEq),
        (">=", Operator::Gte),
        ("<=", Operator::Lte),
        (">", Operator::Gt),
        ("<", Operator::Lt),
    ] {
        if let Some(version) = part.strip_prefix(raw_op) {
            return Ok((operator, Version::parse(version.trim())?));
        }
    }
    Err(Error::InvalidSpecifier(part.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_applies_conjunctive_specifiers() {
        let set = VersionSet::parse(">=1.2,<2.0,!=1.5").expect("set");
        assert!(set.contains(&Version::parse("1.2.0").expect("version")));
        assert!(!set.contains(&Version::parse("1.5").expect("version")));
        assert!(!set.contains(&Version::parse("2.0").expect("version")));
    }

    #[test]
    fn empty_or_star_is_unbounded() {
        assert!(VersionSet::parse("").expect("empty").is_unbounded());
        assert!(VersionSet::parse("*").expect("star").contains(&Version::parse("99.0").expect("version")));
    }
}
