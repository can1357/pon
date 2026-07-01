use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::names;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Requirement {
    raw: String,
    normalized_name: String,
}

impl Requirement {
    pub fn parse(raw: impl AsRef<str>) -> Result<Self> {
        let raw = raw.as_ref().trim();
        if raw.is_empty() {
            return Err(Error::InvalidRequirement(raw.to_owned()));
        }

        if raw.starts_with('.') || raw.starts_with('/') || raw.contains(std::path::MAIN_SEPARATOR) {
            let normalized_name = path_requirement_name(raw).ok_or_else(|| Error::InvalidRequirement(raw.to_owned()))?;
            return Ok(Self {
                raw: raw.to_owned(),
                normalized_name,
            });
        }

        let name_end = raw
            .char_indices()
            .find_map(|(index, ch)| {
                if matches!(ch, '[' | '<' | '>' | '=' | '!' | '~' | ';' | '@') || ch.is_whitespace() {
                    Some(index)
                } else {
                    None
                }
            })
            .unwrap_or(raw.len());
        let name = &raw[..name_end];
        names::validate(name).map_err(|_| Error::InvalidRequirement(raw.to_owned()))?;

        Ok(Self {
            raw: raw.to_owned(),
            normalized_name: names::normalize(name),
        })
    }

    pub fn for_resolved_package(raw: impl AsRef<str>, normalized_name: impl AsRef<str>) -> Result<Self> {
        let raw = raw.as_ref().trim();
        if raw.is_empty() {
            return Err(Error::InvalidRequirement(raw.to_owned()));
        }
        let normalized_name = normalized_name.as_ref();
        names::validate(normalized_name)?;
        Ok(Self {
            raw: raw.to_owned(),
            normalized_name: names::normalize(normalized_name),
        })
    }
    #[must_use]
    pub fn raw(&self) -> &str {
        &self.raw
    }

    #[must_use]
    pub fn normalized_name(&self) -> &str {
        &self.normalized_name
    }
}

fn path_requirement_name(raw: &str) -> Option<String> {
    let basename = Path::new(raw).file_name()?.to_str()?;
    let distribution = basename
        .strip_suffix(".tar.gz")
        .and_then(|stem| stem.rsplit_once('-').map(|(name, _version)| name))
        .unwrap_or(basename);
    names::validate(distribution).ok()?;
    Some(names::normalize(distribution))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectManifest {
    pub path: PathBuf,
    dependencies: BTreeMap<String, Requirement>,
}

impl ProjectManifest {
    #[must_use]
    pub fn empty(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            dependencies: BTreeMap::new(),
        }
    }

    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        match fs::read_to_string(path) {
            Ok(content) => Self::from_str(path, &content),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::empty(path)),
            Err(error) => Err(error.into()),
        }
    }

    pub fn from_str(path: impl Into<PathBuf>, content: &str) -> Result<Self> {
        let path = path.into();
        let mut manifest = Self::empty(path.clone());
        for raw in parse_dependency_strings(content).map_err(|message| Error::manifest(path.clone(), message))? {
            let requirement = Requirement::parse(&raw)?;
            manifest
                .dependencies
                .insert(requirement.normalized_name.clone(), requirement);
        }
        Ok(manifest)
    }

    #[must_use]
    pub fn dependencies(&self) -> Vec<&Requirement> {
        self.dependencies.values().collect()
    }

    pub fn add(&mut self, requirement: Requirement) -> bool {
        self.dependencies
            .insert(requirement.normalized_name.clone(), requirement)
            .is_none()
    }

    pub fn remove(&mut self, name: &str) -> Result<bool> {
        names::validate(name)?;
        Ok(self.dependencies.remove(&names::normalize(name)).is_some())
    }

    pub fn write(&self) -> Result<()> {
        let existing = match fs::read_to_string(&self.path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error.into()),
        };
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        fs::write(&self.path, write_dependencies_block(&existing, &self.dependencies))?;
        Ok(())
    }
}

pub fn add_dependency(path: impl AsRef<Path>, requirement: impl AsRef<str>) -> Result<bool> {
    let mut manifest = ProjectManifest::read(path.as_ref())?;
    let changed = manifest.add(Requirement::parse(requirement)?);
    manifest.write()?;
    Ok(changed)
}

pub fn remove_dependency(path: impl AsRef<Path>, name: impl AsRef<str>) -> Result<bool> {
    let mut manifest = ProjectManifest::read(path.as_ref())?;
    let changed = manifest.remove(name.as_ref())?;
    manifest.write()?;
    Ok(changed)
}

fn parse_dependency_strings(content: &str) -> std::result::Result<Vec<String>, String> {
    let lines = content.lines().collect::<Vec<_>>();
    let Some(project_index) = lines.iter().position(|line| line.trim() == "[project]") else {
        return Ok(Vec::new());
    };
    let mut index = project_index + 1;
    while index < lines.len() {
        let trimmed = lines[index].trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            return Ok(Vec::new());
        }
        if trimmed.starts_with("dependencies") {
            let mut array = String::new();
            let mut cursor = index;
            loop {
                let line = lines
                    .get(cursor)
                    .ok_or_else(|| "unterminated project.dependencies array".to_owned())?;
                array.push_str(line);
                array.push('\n');
                if line.contains(']') {
                    break;
                }
                cursor += 1;
            }
            return extract_string_literals(&array);
        }
        index += 1;
    }
    Ok(Vec::new())
}

fn extract_string_literals(input: &str) -> std::result::Result<Vec<String>, String> {
    let mut values = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;

    for ch in input.chars() {
        match quote {
            Some(_) if escaped => {
                current.push(ch);
                escaped = false;
            }
            Some(_) if ch == '\\' => escaped = true,
            Some(active) if ch == active => {
                values.push(current.clone());
                current.clear();
                quote = None;
            }
            Some(_) => current.push(ch),
            None if ch == '\'' || ch == '"' => quote = Some(ch),
            None => {}
        }
    }

    if quote.is_some() {
        Err("unterminated string in project.dependencies".to_owned())
    } else {
        Ok(values)
    }
}

fn write_dependencies_block(content: &str, deps: &BTreeMap<String, Requirement>) -> String {
    let block = render_dependencies(deps);
    let mut lines = content.lines().map(str::to_owned).collect::<Vec<_>>();

    if let Some(project_index) = lines.iter().position(|line| line.trim() == "[project]") {
        if let Some((start, end)) = dependencies_range(&lines, project_index + 1) {
            lines.splice(start..=end, block.lines().map(str::to_owned));
        } else {
            lines.splice(project_index + 1..project_index + 1, block.lines().map(str::to_owned));
        }
        let mut out = lines.join("\n");
        out.push('\n');
        return out;
    }

    let mut out = String::new();
    if !content.trim().is_empty() {
        out.push_str(content.trim_end());
        out.push_str("\n\n");
    }
    out.push_str("[project]\n");
    out.push_str(&block);
    out
}

fn dependencies_range(lines: &[String], mut index: usize) -> Option<(usize, usize)> {
    while index < lines.len() {
        let trimmed = lines[index].trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            return None;
        }
        if trimmed.starts_with("dependencies") {
            let mut end = index;
            while end < lines.len() && !lines[end].contains(']') {
                end += 1;
            }
            return Some((index, end.min(lines.len() - 1)));
        }
        index += 1;
    }
    None
}

fn render_dependencies(deps: &BTreeMap<String, Requirement>) -> String {
    let mut out = String::from("dependencies = [\n");
    for requirement in deps.values() {
        out.push_str("    \"");
        out.push_str(&escape_toml_string(requirement.raw()));
        out.push_str("\",\n");
    }
    out.push_str("]\n");
    out
}

fn escape_toml_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let unique = format!(
            "pon-pm-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        );
        std::env::temp_dir().join(unique).join("pyproject.toml")
    }

    #[test]
    fn reads_project_dependencies() {
        let content = r#"[project]
name = "demo"
dependencies = [
    "Requests>=2",
    'friendly_bard',
]
"#;
        let manifest = ProjectManifest::from_str("pyproject.toml", content).expect("manifest");
        let deps = manifest
            .dependencies()
            .iter()
            .map(|req| (req.normalized_name(), req.raw()))
            .collect::<Vec<_>>();
        assert_eq!(deps, vec![("friendly-bard", "friendly_bard"), ("requests", "Requests>=2")]);
    }

    #[test]
    fn add_and_remove_dependency_rewrites_project_block() {
        let path = temp_path("rewrite");
        fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        fs::write(&path, "[build-system]\nrequires = []\n\n[project]\nname = \"demo\"\n").expect("write");

        assert!(add_dependency(&path, "Requests>=2").expect("add"));
        assert!(add_dependency(&path, "friendly_bard").expect("add second"));
        let content = fs::read_to_string(&path).expect("content");
        assert!(content.contains("[build-system]\nrequires = []"));
        assert!(content.contains("[project]\ndependencies = [\n    \"friendly_bard\",\n    \"Requests>=2\",\n]\nname = \"demo\""));

        assert!(remove_dependency(&path, "requests").expect("remove"));
        let manifest = ProjectManifest::read(&path).expect("read");
        assert_eq!(manifest.dependencies()[0].normalized_name(), "friendly-bard");
    }

    #[test]
    fn replacing_same_normalized_name_is_not_additive() {
        let mut manifest = ProjectManifest::empty("pyproject.toml");
        assert!(manifest.add(Requirement::parse("Requests>=2").expect("req")));
        assert!(!manifest.add(Requirement::parse("requests>=3").expect("req")));
        assert_eq!(manifest.dependencies()[0].raw(), "requests>=3");
    }
}
