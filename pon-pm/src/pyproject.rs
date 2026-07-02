use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, TableLike, Value, value};

use crate::error::{Error, Result};
use crate::names;

#[derive(Clone)]
pub struct PyProject {
    doc: DocumentMut,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildSystem {
    pub requires: Vec<String>,
    pub build_backend: Option<String>,
    pub backend_path: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PonSource {
    pub path: Option<PathBuf>,
    pub editable: bool,
    pub git: Option<String>,
    pub rev: Option<String>,
}

impl PyProject {
    #[must_use]
    pub fn empty(path: impl Into<PathBuf>) -> Self {
        Self {
            doc: DocumentMut::new(),
            path: path.into(),
        }
    }

    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        match fs::read_to_string(path) {
            Ok(content) => Self::from_str(path, &content),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::from_str(path, ""),
            Err(error) => Err(error.into()),
        }
    }

    pub fn from_str(path: impl Into<PathBuf>, s: &str) -> Result<Self> {
        let path = path.into();
        let doc = s
            .parse::<DocumentMut>()
            .map_err(|error| Error::manifest(path.clone(), error.to_string()))?;
        let pyproject = Self { doc, path };
        pyproject.parse_sources()?;
        Ok(pyproject)
    }

    #[must_use]
    pub fn project_name(&self) -> Option<&str> {
        self.string_at(&["project", "name"])
    }

    #[must_use]
    pub fn project_version(&self) -> Option<&str> {
        self.string_at(&["project", "version"])
    }

    #[must_use]
    pub fn dependencies(&self) -> Vec<String> {
        self.string_array_at(&["project", "dependencies"])
    }

    #[must_use]
    pub fn optional_dependencies(&self) -> BTreeMap<String, Vec<String>> {
        let mut optional = BTreeMap::new();
        let Some(table) = self.table_like_at(&["project", "optional-dependencies"]) else {
            return optional;
        };
        for (key, item) in table.iter() {
            let deps = item
                .as_array()
                .map(|array| string_array_values(array).collect::<Vec<_>>())
                .unwrap_or_default();
            optional.insert(key.get().to_owned(), deps);
        }
        optional
    }

    #[must_use]
    pub fn build_system(&self) -> Option<BuildSystem> {
        self.table_like_at(&["build-system"]).map(|table| BuildSystem {
            requires: table
                .get("requires")
                .and_then(Item::as_array)
                .map(|array| string_array_values(array).collect::<Vec<_>>())
                .unwrap_or_default(),
            build_backend: table.get("build-backend").and_then(Item::as_str).map(str::to_owned),
            backend_path: table
                .get("backend-path")
                .and_then(Item::as_array)
                .map(|array| string_array_values(array).collect::<Vec<_>>())
                .unwrap_or_default(),
        })
    }

    pub(crate) fn build_system_has_key(&self, key: &str) -> bool {
        self.table_like_at(&["build-system"])
            .is_some_and(|table| table.contains_key(key))
    }

    #[must_use]
    pub fn tool_pon_index_url(&self) -> Option<&str> {
        self.string_at(&["tool", "pon", "index-url"])
    }

    #[must_use]
    pub fn tool_pon_allow_prerelease(&self) -> bool {
        self.bool_at(&["tool", "pon", "allow-prerelease"]).unwrap_or(false)
    }

    #[must_use]
    pub fn tool_pon_import_name(&self) -> Option<&str> {
        self.string_at(&["tool", "pon", "import-name"])
    }

    #[must_use]
    pub fn tool_pon_native_import_name(&self) -> Option<&str> {
        self.string_at(&["tool", "pon", "native", "import-name"])
    }

    #[must_use]
    pub fn sources(&self) -> BTreeMap<String, PonSource> {
        self.parse_sources().unwrap_or_default()
    }

    pub fn set_source(&mut self, name: &str, source: &PonSource) {
        let normalized = names::normalize(name);
        let mut table = InlineTable::new();
        if let Some(path) = &source.path {
            table.insert("path", Value::from(self.source_path_for_write(path)));
            if source.editable {
                table.insert("editable", Value::from(true));
            }
        }
        if let Some(git) = &source.git {
            table.insert("git", Value::from(git.as_str()));
        }
        if let Some(rev) = &source.rev {
            table.insert("rev", Value::from(rev.as_str()));
        }
        table.fmt();
        let sources = ensure_table_path(self.doc.as_item_mut(), &["tool", "pon", "sources"]);
        sources.insert(&normalized, value(table));
    }

    pub fn remove_source(&mut self, name: &str) -> bool {
        let normalized = names::normalize(name);
        let Some(sources) = item_at_mut(self.doc.as_item_mut(), &["tool", "pon", "sources"])
            .and_then(Item::as_table_like_mut)
        else {
            return false;
        };
        sources.remove(&normalized).is_some()
    }

    pub fn add_dependency(&mut self, raw: &str) -> Result<bool> {
        let normalized = dependency_normalized_name(raw)?;
        let mut dependencies = self.dependency_map()?;
        let added = dependencies.insert(normalized, raw.trim().to_owned()).is_none();
        self.set_dependency_strings(dependencies.values().map(String::as_str));
        Ok(added)
    }

    pub fn remove_dependency(&mut self, name: &str) -> Result<bool> {
        names::validate(name)?;
        let mut dependencies = self.dependency_map()?;
        let removed = dependencies.remove(&names::normalize(name)).is_some();
        self.set_dependency_strings(dependencies.values().map(String::as_str));
        Ok(removed)
    }

    pub fn write(&self) -> Result<()> {
        self.parse_sources()?;
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        fs::write(&self.path, self.doc.to_string())?;
        Ok(())
    }

    pub(crate) fn set_dependency_strings<'a>(&mut self, dependencies: impl IntoIterator<Item = &'a str>) {
        let mut array = Array::new();
        for dependency in dependencies {
            array.push(dependency);
        }
        let project = ensure_table_path(self.doc.as_item_mut(), &["project"]);
        project.insert("dependencies", value(array));
    }

    fn dependency_map(&self) -> Result<BTreeMap<String, String>> {
        let mut dependencies = BTreeMap::new();
        for raw in self.dependencies() {
            dependencies.insert(dependency_normalized_name(&raw)?, raw);
        }
        Ok(dependencies)
    }

    fn parse_sources(&self) -> Result<BTreeMap<String, PonSource>> {
        let mut sources = BTreeMap::new();
        let Some(table) = self.table_like_at(&["tool", "pon", "sources"]) else {
            return Ok(sources);
        };
        for (key, item) in table.iter() {
            if item.is_none() {
                continue;
            }
            let source_name = names::normalize(key.get());
            let Some(entry) = item.as_table_like() else {
                return Err(Error::manifest(
                    self.path.clone(),
                    format!("[tool.pon.sources].{} must be a table", key.get()),
                ));
            };
            let raw_path = entry.get("path").and_then(Item::as_str);
            let raw_git = entry.get("git").and_then(Item::as_str);
            if raw_path.is_some() == raw_git.is_some() {
                return Err(Error::manifest(
                    self.path.clone(),
                    format!("[tool.pon.sources].{} must specify exactly one of `path` or `git`", key.get()),
                ));
            }
            if raw_path.is_none() && entry.contains_key("editable") {
                return Err(Error::manifest(
                    self.path.clone(),
                    format!("[tool.pon.sources].{} sets `editable` without `path`", key.get()),
                ));
            }
            let path = raw_path.map(|path| self.resolve_source_path(path));
            let editable = entry.get("editable").and_then(Item::as_bool).unwrap_or(false);
            let git = raw_git.map(str::to_owned);
            let rev = entry.get("rev").and_then(Item::as_str).map(str::to_owned);
            sources.insert(
                source_name,
                PonSource {
                    path,
                    editable,
                    git,
                    rev,
                },
            );
        }
        Ok(sources)
    }

    fn string_at(&self, path: &[&str]) -> Option<&str> {
        item_at(self.doc.as_item(), path).and_then(Item::as_str)
    }

    fn bool_at(&self, path: &[&str]) -> Option<bool> {
        item_at(self.doc.as_item(), path).and_then(Item::as_bool)
    }

    fn string_array_at(&self, path: &[&str]) -> Vec<String> {
        item_at(self.doc.as_item(), path)
            .and_then(Item::as_array)
            .map(|array| string_array_values(array).collect())
            .unwrap_or_default()
    }

    fn table_like_at(&self, path: &[&str]) -> Option<&dyn TableLike> {
        item_at(self.doc.as_item(), path).and_then(Item::as_table_like)
    }

    fn base_dir(&self) -> PathBuf {
        self.path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    }

    fn resolve_source_path(&self, raw: &str) -> PathBuf {
        let path = PathBuf::from(raw);
        if path.is_absolute() {
            path
        } else {
            self.base_dir().join(path)
        }
    }

    fn source_path_for_write(&self, path: &Path) -> String {
        let base = self.base_dir();
        let relative = path.strip_prefix(&base).ok().filter(|relative| !relative.as_os_str().is_empty());
        relative.unwrap_or(path).to_string_lossy().into_owned()
    }
}

fn item_at<'a>(item: &'a Item, path: &[&str]) -> Option<&'a Item> {
    let mut current = item;
    for key in path {
        current = current.as_table_like()?.get(key)?;
    }
    Some(current)
}

fn item_at_mut<'a>(item: &'a mut Item, path: &[&str]) -> Option<&'a mut Item> {
    let mut current = item;
    for key in path {
        current = current.get_mut(*key)?;
    }
    Some(current)
}

fn ensure_table_path<'a>(item: &'a mut Item, path: &[&str]) -> &'a mut Table {
    if path.is_empty() {
        if !item.is_table() {
            *item = Item::Table(Table::new());
        }
        return item.as_table_mut().expect("ensured table");
    }
    if !item.is_table() {
        *item = Item::Table(Table::new());
    }
    let table = item.as_table_mut().expect("ensured table");
    let child = table.entry(path[0]).or_insert(Item::Table(Table::new()));
    ensure_table_path(child, &path[1..])
}

fn string_array_values(array: &Array) -> impl Iterator<Item = String> + '_ {
    array.iter().filter_map(|value| value.as_str().map(str::to_owned))
}

fn dependency_normalized_name(raw: &str) -> Result<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(Error::InvalidRequirement(raw.to_owned()));
    }

    if raw.starts_with('.') || raw.starts_with('/') || raw.contains(std::path::MAIN_SEPARATOR) {
        return path_requirement_name(raw).ok_or_else(|| Error::InvalidRequirement(raw.to_owned()));
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
    Ok(names::normalize(name))
}

fn path_requirement_name(raw: &str) -> Option<String> {
    let basename = Path::new(raw).file_name()?.to_str()?;
    let distribution = basename
        .strip_suffix(".tar.gz")
        .or_else(|| basename.strip_suffix(".zip"))
        .or_else(|| basename.strip_suffix(".whl"))
        .and_then(|stem| stem.rsplit_once('-').map(|(name, _version)| name))
        .unwrap_or(basename);
    names::validate(distribution).ok()?;
    Some(names::normalize(distribution))
}
