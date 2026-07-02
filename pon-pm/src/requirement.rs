use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::error::{Error, Result};
use crate::names;
use crate::pyproject::PyProject;
use crate::wheel::filename::WheelFilename;

/// A requirement supplied by a user, manifest, or requirements file before it is
/// resolved into concrete candidates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RequirementInput {
    /// Full PEP 508 input: `name[extras]`, specifiers or `name @ url`, and an
    /// optional environment marker.
    Pep508(pep508_rs::Requirement),
    /// A local file or directory requirement. The parser leaves `editable`
    /// false; callers that process `-e`/`--editable` set it after parsing.
    Path { path: PathBuf, editable: bool },
    /// A bare URL without the `name @` prefix.
    Url { url: pep508_rs::VerbatimUrl },
}

impl RequirementInput {
    /// Return `true` when this input is a full PEP 508 requirement.
    pub fn is_pep508(&self) -> bool {
        matches!(self, Self::Pep508(_))
    }

    /// Return the parsed PEP 508 requirement, if this input is PEP 508.
    pub fn as_pep508(&self) -> Option<&pep508_rs::Requirement> {
        match self {
            Self::Pep508(requirement) => Some(requirement),
            _ => None,
        }
    }

    /// Return `true` when this input is a local path or archive requirement.
    pub fn is_path(&self) -> bool {
        matches!(self, Self::Path { .. })
    }

    /// Return the local path and editable flag, if this input is path-backed.
    pub fn as_path(&self) -> Option<(&Path, bool)> {
        match self {
            Self::Path { path, editable } => Some((path.as_path(), *editable)),
            _ => None,
        }
    }

    /// Return `true` when this input is a bare URL requirement.
    pub fn is_url(&self) -> bool {
        matches!(self, Self::Url { .. })
    }

    /// Return the bare URL, if this input was supplied without a `name @` prefix.
    pub fn as_url(&self) -> Option<&pep508_rs::VerbatimUrl> {
        match self {
            Self::Url { url } => Some(url),
            _ => None,
        }
    }

    /// Return `true` when this input is a local path marked editable.
    pub fn is_editable(&self) -> bool {
        matches!(self, Self::Path { editable: true, .. })
    }

    /// Set the editable flag on a local path requirement.
    ///
    /// Returns `true` when the input is path-backed. For PEP 508 and bare URL
    /// inputs the value is left unchanged and `false` is returned so callers can
    /// report the appropriate CLI or requirements-file error.
    pub fn set_editable(&mut self, editable: bool) -> bool {
        if let Self::Path { editable: flag, .. } = self {
            *flag = editable;
            true
        } else {
            false
        }
    }
}

/// Parse one raw requirement string using pon's Phase-0 classification order.
///
/// Bare HTTP(S), `file://`, and `git+` strings become [`RequirementInput::Url`].
/// Named PEP 508 direct URL requirements (`name @ url`) stay PEP 508 even
/// though the URL contains path separators. Other path-looking strings become
/// [`RequirementInput::Path`]. Everything else is parsed as a PEP 508
/// requirement with `pep508_rs`.
pub fn parse_requirement_input(raw: &str) -> Result<RequirementInput> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(Error::InvalidRequirement(raw.to_owned()));
    }

    if is_bare_url(raw) {
        let url = pep508_rs::VerbatimUrl::from_str(raw)
            .map_err(|_| Error::InvalidRequirement(raw.to_owned()))?;
        return Ok(RequirementInput::Url { url });
    }

    if is_pep508_direct_url(raw) {
        return parse_pep508_requirement(raw);
    }

    if looks_like_path(raw) || is_existing_archive_path(raw) {
        return Ok(RequirementInput::Path {
            path: PathBuf::from(raw),
            editable: false,
        });
    }

    parse_pep508_requirement(raw)
}

/// Return the PEP 503-normalized distribution name for a requirement input.
///
/// `project_root` is the base directory used to resolve relative local path
/// requirements before reading `<path>/pyproject.toml`. PEP 508 requirements
/// and URL requirements do not use this context.
pub fn normalized_name_of(
    input: &RequirementInput,
    project_root: impl AsRef<Path>,
) -> Result<String> {
    match input {
        RequirementInput::Pep508(requirement) => Ok(requirement.name.as_ref().to_owned()),
        RequirementInput::Path { path, .. } => normalized_name_of_path(path, project_root.as_ref()),
        RequirementInput::Url { url } => normalized_name_of_url(url),
    }
}

fn is_bare_url(raw: &str) -> bool {
    raw.starts_with("http://")
        || raw.starts_with("https://")
        || raw.starts_with("file://")
        || raw.starts_with("git+")
}

fn is_pep508_direct_url(raw: &str) -> bool {
    let Some((name, url)) = raw.split_once('@') else {
        return false;
    };
    let name = name.trim();
    let url = url.trim_start();
    !name.is_empty() && !looks_like_path(name) && is_bare_url(url)
}

fn looks_like_path(raw: &str) -> bool {
    raw.starts_with('.')
        || raw.starts_with('/')
        || raw.starts_with('~')
        || raw.contains(std::path::MAIN_SEPARATOR)
}

fn is_existing_archive_path(raw: &str) -> bool {
    archive_kind(raw).is_some() && Path::new(raw).exists()
}

fn parse_pep508_requirement(raw: &str) -> Result<RequirementInput> {
    pep508_rs::Requirement::from_str(raw)
        .map(RequirementInput::Pep508)
        .map_err(|_| Error::InvalidRequirement(raw.to_owned()))
}

fn normalized_name_of_path(path: &Path, project_root: &Path) -> Result<String> {
    if archive_kind(path_to_str(path)?).is_some() {
        return normalized_name_from_archive_path(path);
    }

    let resolved = resolve_against(project_root, path);
    let pyproject = PyProject::read(resolved.join("pyproject.toml"))?;
    let name = pyproject.project_name().ok_or_else(|| {
        Error::InvalidRequirement(format!(
            "local package `{}` is missing [project].name",
            resolved.display()
        ))
    })?;
    normalized_validated_name(name).map_err(|_| {
        Error::InvalidRequirement(format!(
            "local package `{}` has invalid [project].name `{name}`",
            resolved.display()
        ))
    })
}

fn normalized_name_of_url(url: &pep508_rs::VerbatimUrl) -> Result<String> {
    let basename = url
        .raw()
        .path_segments()
        .and_then(|segments| segments.rev().find(|segment| !segment.is_empty()))
        .ok_or_else(|| Error::InvalidRequirement(url.to_string()))?;

    normalized_name_from_filename(basename).map_err(|_| Error::InvalidRequirement(url.to_string()))
}

fn normalized_name_from_archive_path(path: &Path) -> Result<String> {
    let basename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::InvalidRequirement(path.display().to_string()))?;
    normalized_name_from_filename(basename)
}

fn normalized_name_from_filename(filename: &str) -> Result<String> {
    if filename.ends_with(".whl") {
        return WheelFilename::parse(filename)
            .map(|wheel| wheel.normalized_distribution)
            .map_err(|_| Error::InvalidRequirement(filename.to_owned()));
    }

    normalized_name_from_sdist_or_basename(filename)
}

fn normalized_name_from_sdist_or_basename(filename: &str) -> Result<String> {
    let distribution = filename
        .strip_suffix(".tar.gz")
        .or_else(|| filename.strip_suffix(".zip"))
        .and_then(|stem| stem.rsplit_once('-').map(|(name, _version)| name))
        .unwrap_or(filename);

    normalized_validated_name(distribution)
        .map_err(|_| Error::InvalidRequirement(filename.to_owned()))
}

fn normalized_validated_name(name: &str) -> Result<String> {
    names::validate(name)?;
    Ok(names::normalize(name))
}

fn resolve_against(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn path_to_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| Error::InvalidRequirement(path.display().to_string()))
}

fn archive_kind(raw: &str) -> Option<ArchiveKind> {
    if raw.ends_with(".whl") {
        Some(ArchiveKind::Wheel)
    } else if raw.ends_with(".tar.gz") || raw.ends_with(".zip") {
        Some(ArchiveKind::Sdist)
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArchiveKind {
    Wheel,
    Sdist,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn classifies_full_pep508_requirement() {
        let input = parse_requirement_input("pkg[extra1,extra2]>=1; python_version >= '3.12'").expect("requirement");

        let RequirementInput::Pep508(requirement) = input else {
            panic!("expected PEP 508 requirement");
        };
        assert_eq!(requirement.name.as_ref(), "pkg");
        assert_eq!(requirement.extras.len(), 2);
    }

    #[test]
    fn classifies_pep508_direct_url_requirement() {
        let input = parse_requirement_input("demo @ https://example.com/demo-1.0-py3-none-any.whl").expect("requirement");

        let RequirementInput::Pep508(requirement) = input else {
            panic!("expected PEP 508 requirement");
        };
        assert_eq!(requirement.name.as_ref(), "demo");
    }

    #[test]
    fn classifies_bare_http_url() {
        let input = parse_requirement_input("https://example.com/demo-1.0-py3-none-any.whl").expect("url");

        assert!(matches!(input, RequirementInput::Url { .. }));
    }

    #[test]
    fn classifies_bare_git_url() {
        let input = parse_requirement_input("git+https://example.com/org/demo.git@v1#subdirectory=sub").expect("url");

        assert!(matches!(input, RequirementInput::Url { .. }));
    }

    #[test]
    fn classifies_local_path() {
        let input = parse_requirement_input("./vendor/pkg").expect("path");

        let RequirementInput::Path { path, editable } = input else {
            panic!("expected path requirement");
        };
        assert_eq!(path, PathBuf::from("./vendor/pkg"));
        assert!(!editable);
    }

    #[test]
    fn exposes_variant_helpers() {
        let pep508 = parse_requirement_input("demo>=1").expect("requirement");
        assert!(pep508.is_pep508());
        assert!(pep508.as_pep508().is_some());
        assert!(!pep508.is_path());
        assert!(pep508.as_path().is_none());
        assert!(!pep508.is_url());
        assert!(pep508.as_url().is_none());

        let mut path_input = parse_requirement_input("./vendor/pkg").expect("path");
        let (path, editable) = path_input.as_path().expect("path helper");
        assert_eq!(path, Path::new("./vendor/pkg"));
        assert!(!editable);
        assert!(path_input.set_editable(true));
        assert!(path_input.is_path());
        assert!(path_input.is_editable());

        let mut url_input = parse_requirement_input("https://example.com/demo-1.0.zip").expect("url");
        assert!(url_input.is_url());
        assert!(url_input.as_url().is_some());
        assert!(!url_input.set_editable(true));
        assert!(!url_input.is_editable());
    }

    #[test]
    fn classifies_existing_archive_path() {
        let root = temp_dir("existing-archive");
        let archive = root.join("pkg-1.0.tar.gz");
        fs::write(&archive, b"").expect("write archive");

        let input = parse_requirement_input(archive.to_str().expect("utf8 path")).expect("path");

        assert!(matches!(input, RequirementInput::Path { editable: false, .. }));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn normalizes_pep508_name() {
        let input = parse_requirement_input("Friendly_Bard>=1").expect("requirement");

        assert_eq!(normalized_name_of(&input, ".").expect("name"), "friendly-bard");
    }

    #[test]
    fn normalizes_directory_name_from_pyproject() {
        let root = temp_dir("path-name");
        let package = root.join("vendor/pkg");
        fs::create_dir_all(&package).expect("create package");
        fs::write(
            package.join("pyproject.toml"),
            "[project]\nname = \"Friendly_Bard\"\nversion = \"1.0\"\n",
        )
        .expect("write pyproject");
        let input = RequirementInput::Path {
            path: PathBuf::from("vendor/pkg"),
            editable: false,
        };

        assert_eq!(normalized_name_of(&input, &root).expect("name"), "friendly-bard");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn normalizes_wheel_archive_name() {
        let input = RequirementInput::Path {
            path: PathBuf::from("Friendly_Bard-1.2.3-py3-none-any.whl"),
            editable: false,
        };

        assert_eq!(normalized_name_of(&input, ".").expect("name"), "friendly-bard");
    }

    #[test]
    fn normalizes_sdist_archive_name() {
        let input = RequirementInput::Path {
            path: PathBuf::from("Friendly_Bard-1.2.3.tar.gz"),
            editable: false,
        };

        assert_eq!(normalized_name_of(&input, ".").expect("name"), "friendly-bard");
    }

    #[test]
    fn normalizes_bare_url_archive_name() {
        let input = parse_requirement_input("https://example.com/packages/Friendly_Bard-1.2.3.zip#sha256=abc").expect("url");

        assert_eq!(normalized_name_of(&input, ".").expect("name"), "friendly-bard");
    }

    fn temp_dir(prefix: &str) -> PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("pon-pm-requirement-{prefix}-{id}"));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }
}
