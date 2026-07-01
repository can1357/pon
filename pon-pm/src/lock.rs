use std::fmt;
use std::fs;
use std::path::Path;
use std::str::FromStr;

use crate::error::{Error, Result};
use crate::resolve::source::{PackageKind, PackageRecord};

pub const LOCK_VERSION: &str = "1.0";
pub const CREATED_BY: &str = concat!("pon-pm ", env!("CARGO_PKG_VERSION"));

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockFile {
    pub lock_version: String,
    pub created_by: String,
    pub packages: Vec<LockedPackage>,
}

impl LockFile {
    #[must_use]
    pub fn new(mut packages: Vec<LockedPackage>) -> Self {
        packages.sort_by(|left, right| left.name.cmp(&right.name).then_with(|| left.version.cmp(&right.version)));
        Self {
            lock_version: LOCK_VERSION.to_owned(),
            created_by: CREATED_BY.to_owned(),
            packages,
        }
    }

    #[must_use]
    pub fn from_records(records: &[PackageRecord]) -> Self {
        Self::new(records.iter().map(LockedPackage::from).collect())
    }

    pub fn write_to_path(&self, path: impl AsRef<Path>) -> Result<()> {
        fs::write(path, self.to_string())?;
        Ok(())
    }

    pub fn read_from_path(path: impl AsRef<Path>) -> Result<Self> {
        fs::read_to_string(path)?.parse()
    }
}

impl fmt::Display for LockFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "lock-version = {}", quoted(&self.lock_version))?;
        writeln!(f, "created-by = {}", quoted(&self.created_by))?;
        for package in &self.packages {
            writeln!(f)?;
            writeln!(f, "[[packages]]")?;
            writeln!(f, "name = {}", quoted(&package.name))?;
            writeln!(f, "version = {}", quoted(&package.version))?;
            if let Some(kind) = package.kind.tool_kind() {
                writeln!(f)?;
                writeln!(f, "[packages.tool.pon]")?;
                writeln!(f, "kind = {}", quoted(kind))?;
            }
        }
        Ok(())
    }
}

impl FromStr for LockFile {
    type Err = Error;

    fn from_str(input: &str) -> Result<Self> {
        parse_lock(input)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockedPackage {
    pub name: String,
    pub version: String,
    pub kind: LockedPackageKind,
}

impl LockedPackage {
    #[must_use]
    pub fn pure(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            kind: LockedPackageKind::Pure,
        }
    }

    #[must_use]
    pub fn native(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            kind: LockedPackageKind::Native,
        }
    }

    #[must_use]
    pub fn cabi_refused(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            kind: LockedPackageKind::CAbiRefused,
        }
    }
}

impl From<&PackageRecord> for LockedPackage {
    fn from(record: &PackageRecord) -> Self {
        Self {
            name: record.name.clone(),
            version: record.version.clone(),
            kind: LockedPackageKind::from(&record.kind),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockedPackageKind {
    Pure,
    Native,
    CAbiRefused,
}

impl LockedPackageKind {
    #[must_use]
    pub fn tool_kind(self) -> Option<&'static str> {
        match self {
            Self::Pure => None,
            Self::Native => Some("native"),
            Self::CAbiRefused => Some("cabi-refused"),
        }
    }
}

impl From<&PackageKind> for LockedPackageKind {
    fn from(kind: &PackageKind) -> Self {
        match kind {
            PackageKind::Pure => Self::Pure,
            PackageKind::Native => Self::Native,
            PackageKind::CAbiRefused { .. } => Self::CAbiRefused,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Section {
    Root,
    Package,
    PonTool,
}

#[derive(Default)]
struct PackageBuilder {
    name: Option<String>,
    version: Option<String>,
    kind: Option<LockedPackageKind>,
}

fn parse_lock(input: &str) -> Result<LockFile> {
    let mut lock_version = None;
    let mut created_by = None;
    let mut packages = Vec::new();
    let mut current = None;
    let mut section = Section::Root;

    for line in input.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match line {
            "[[packages]]" => {
                finish_package(&mut packages, current.take())?;
                current = Some(PackageBuilder::default());
                section = Section::Package;
            }
            "[packages.tool.pon]" => {
                if current.is_none() {
                    return Err(Error::InvalidRequirement("lock package tool table before package".to_owned()));
                }
                section = Section::PonTool;
            }
            _ => {
                let (key, value) = key_value(line)?;
                match (section, key) {
                    (Section::Root, "lock-version") => lock_version = Some(value),
                    (Section::Root, "created-by") => created_by = Some(value),
                    (Section::Package, "name") => current_package(&mut current)?.name = Some(value),
                    (Section::Package, "version") => current_package(&mut current)?.version = Some(value),
                    (Section::PonTool, "kind") => current_package(&mut current)?.kind = Some(parse_kind(&value)?),
                    _ => return Err(Error::InvalidRequirement(format!("unsupported pon.lock entry `{line}`"))),
                }
            }
        }
    }
    finish_package(&mut packages, current)?;

    Ok(LockFile {
        lock_version: lock_version.ok_or_else(|| Error::InvalidRequirement("pon.lock missing lock-version".to_owned()))?,
        created_by: created_by.ok_or_else(|| Error::InvalidRequirement("pon.lock missing created-by".to_owned()))?,
        packages,
    })
}

fn finish_package(packages: &mut Vec<LockedPackage>, builder: Option<PackageBuilder>) -> Result<()> {
    if let Some(builder) = builder {
        packages.push(LockedPackage {
            name: builder
                .name
                .ok_or_else(|| Error::InvalidRequirement("pon.lock package missing name".to_owned()))?,
            version: builder
                .version
                .ok_or_else(|| Error::InvalidRequirement("pon.lock package missing version".to_owned()))?,
            kind: builder.kind.unwrap_or(LockedPackageKind::Pure),
        });
    }
    Ok(())
}

fn current_package(current: &mut Option<PackageBuilder>) -> Result<&mut PackageBuilder> {
    current
        .as_mut()
        .ok_or_else(|| Error::InvalidRequirement("pon.lock package field before package".to_owned()))
}

fn key_value(line: &str) -> Result<(&str, String)> {
    let (key, value) = line
        .split_once('=')
        .ok_or_else(|| Error::InvalidRequirement(format!("invalid pon.lock entry `{line}`")))?;
    Ok((key.trim(), unquoted(value.trim())?))
}

fn parse_kind(value: &str) -> Result<LockedPackageKind> {
    match value {
        "native" => Ok(LockedPackageKind::Native),
        "cabi-refused" => Ok(LockedPackageKind::CAbiRefused),
        "pure" => Ok(LockedPackageKind::Pure),
        _ => Err(Error::InvalidRequirement(format!("unsupported pon package kind `{value}`"))),
    }
}

fn quoted(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => output.push_str("\\\\"),
            '"' => output.push_str("\\\""),
            _ => output.push(ch),
        }
    }
    output.push('"');
    output
}

fn unquoted(value: &str) -> Result<String> {
    let Some(value) = value.strip_prefix('"').and_then(|value| value.strip_suffix('"')) else {
        return Err(Error::InvalidRequirement(format!("pon.lock value is not a string: `{value}`")));
    };
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let escaped = chars
                .next()
                .ok_or_else(|| Error::InvalidRequirement("unterminated pon.lock string escape".to_owned()))?;
            output.push(escaped);
        } else {
            output.push(ch);
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_pep_751_shaped_lock_with_tool_metadata_only_when_needed() {
        let lock = LockFile::new(vec![
            LockedPackage::native("fastjson", "0.1.0"),
            LockedPackage::pure("idna", "3.10"),
            LockedPackage::cabi_refused("numpy", "2.3.1"),
        ]);

        assert_eq!(lock.to_string(), concat!(
            "lock-version = \"1.0\"\n",
            "created-by = \"pon-pm 0.1.0\"\n",
            "\n",
            "[[packages]]\n",
            "name = \"fastjson\"\n",
            "version = \"0.1.0\"\n",
            "\n",
            "[packages.tool.pon]\n",
            "kind = \"native\"\n",
            "\n",
            "[[packages]]\n",
            "name = \"idna\"\n",
            "version = \"3.10\"\n",
            "\n",
            "[[packages]]\n",
            "name = \"numpy\"\n",
            "version = \"2.3.1\"\n",
            "\n",
            "[packages.tool.pon]\n",
            "kind = \"cabi-refused\"\n",
        ));
    }

    #[test]
    fn reads_written_lock_file() {
        let lock = LockFile::new(vec![
            LockedPackage::pure("idna", "3.10"),
            LockedPackage::native("fastjson", "0.1.0"),
        ]);
        let path = std::env::temp_dir().join(format!(
            "pon-lock-test-{}-{}.lock",
            std::process::id(),
            std::thread::current().name().unwrap_or("unnamed")
        ));

        lock.write_to_path(&path).expect("write lock");
        let read_back = LockFile::read_from_path(&path).expect("read lock");
        let _ = fs::remove_file(path);

        assert_eq!(read_back, lock);
    }
}
