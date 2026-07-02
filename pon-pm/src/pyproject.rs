use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, TableLike, Value, value};

use crate::error::{Error, Result};
use crate::names;
use crate::requirement::{normalized_name_of, parse_requirement_input};

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
            optional.insert(key.to_owned(), deps);
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
        let Some(sources) = item_at_mut_existing(self.doc.as_item_mut(), &["tool", "pon", "sources"])
            .and_then(Item::as_table_like_mut)
        else {
            return false;
        };
        sources.remove(&normalized).is_some()
    }

    pub fn add_dependency(&mut self, raw: &str) -> Result<bool> {
        let normalized = dependency_normalized_name(raw, &self.base_dir())?;
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
            dependencies.insert(dependency_normalized_name(&raw, &self.base_dir())?, raw);
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
            let source_name = names::normalize(key);
            let Some(entry) = item.as_table_like() else {
                return Err(Error::manifest(
                    self.path.clone(),
                    format!("[tool.pon.sources].{} must be a table", key),
                ));
            };
            let raw_path = entry.get("path").and_then(Item::as_str);
            let raw_git = entry.get("git").and_then(Item::as_str);
            if raw_path.is_some() == raw_git.is_some() {
                return Err(Error::manifest(
                    self.path.clone(),
                    format!("[tool.pon.sources].{} must specify exactly one of `path` or `git`", key),
                ));
            }
            if raw_path.is_none() && entry.contains_key("editable") {
                return Err(Error::manifest(
                    self.path.clone(),
                    format!("[tool.pon.sources].{} sets `editable` without `path`", key),
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
        if !path.is_absolute() {
            return path.to_string_lossy().into_owned();
        }
        let base = self.base_dir();
        let absolute_base = if base.is_absolute() {
            base
        } else {
            std::env::current_dir()
                .map(|current| current.join(base))
                .unwrap_or_else(|_| PathBuf::from("."))
        };
        let relative = path.strip_prefix(&absolute_base).ok().filter(|relative| !relative.as_os_str().is_empty());
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

fn item_at_mut_existing<'a>(item: &'a mut Item, path: &[&str]) -> Option<&'a mut Item> {
    let mut current = item;
    for key in path {
        current = current.as_table_like_mut()?.get_mut(key)?;
    }
    Some(current)
}

fn ensure_table_path<'a>(item: &'a mut Item, path: &[&str]) -> &'a mut Table {
    let table = ensure_table_item(item);
    if path.is_empty() {
        return table;
    }
    let child = table.entry(path[0]).or_insert(Item::Table(Table::new()));
    ensure_table_path(child, &path[1..])
}

fn ensure_table_item(item: &mut Item) -> &mut Table {
    if !item.is_table() {
        let previous = std::mem::take(item);
        *item = previous.into_table().map(Item::Table).unwrap_or_else(|_| Item::Table(Table::new()));
    }
    item.as_table_mut().expect("ensured table")
}

fn string_array_values(array: &Array) -> impl Iterator<Item = String> + '_ {
    array.iter().filter_map(|value| value.as_str().map(str::to_owned))
}

fn dependency_normalized_name(raw: &str, base_dir: &Path) -> Result<String> {
    let input = parse_requirement_input(raw.trim())?;
    normalized_name_of(&input, base_dir)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use crate::requirement::{normalized_name_of, parse_requirement_input};

    use super::PyProject;

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

    fn normalized_name(raw: &str) -> String {
        let input = parse_requirement_input(raw).expect("requirement");
        normalized_name_of(&input, Path::new(".")).expect("normalized name")
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
        let pyproject = PyProject::from_str("pyproject.toml", content).expect("pyproject");
        let mut deps = pyproject
            .dependencies()
            .into_iter()
            .map(|raw| (normalized_name(&raw), raw))
            .collect::<Vec<_>>();
        deps.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(
            deps,
            vec![
                ("friendly-bard".to_owned(), "friendly_bard".to_owned()),
                ("requests".to_owned(), "Requests>=2".to_owned()),
            ]
        );
    }

    #[test]
    fn add_and_remove_dependency_rewrites_project_block() {
        let path = temp_path("rewrite");
        fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        fs::write(
            &path,
            "[build-system]\nrequires = []\n\n[project]\nname = \"demo\"\n",
        )
        .expect("write");

        let mut pyproject = PyProject::read(&path).expect("read");
        assert!(pyproject.add_dependency("Requests>=2").expect("add"));
        assert!(pyproject.add_dependency("friendly_bard").expect("add second"));
        pyproject.write().expect("write");

        let content = fs::read_to_string(&path).expect("content");
        assert!(content.contains("[build-system]\nrequires = []"));
        assert!(content.contains("name = \"demo\""));
        assert!(content.contains("\"friendly_bard\""));
        assert!(content.contains("\"Requests>=2\""));

        let mut pyproject = PyProject::read(&path).expect("read");
        assert!(pyproject.remove_dependency("requests").expect("remove"));
        pyproject.write().expect("write");

        let pyproject = PyProject::read(&path).expect("read");
        let deps = pyproject.dependencies();
        assert_eq!(deps.len(), 1);
        assert_eq!(normalized_name(&deps[0]), "friendly-bard");
    }

    #[test]
    fn replacing_same_normalized_name_is_not_additive() {
        let mut pyproject = PyProject::empty("pyproject.toml");
        assert!(pyproject.add_dependency("Requests>=2").expect("add"));
        assert!(!pyproject.add_dependency("requests>=3").expect("replace"));
        assert_eq!(pyproject.dependencies(), vec!["requests>=3".to_owned()]);
    }
}

