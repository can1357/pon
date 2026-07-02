//! Conservative module reachability and dynamic-code policy for AoT builds.
//!
//! Phase C deliberately computes a module closure, not a Python call graph. Once
//! a module is statically reachable, every lowered function in that module stays
//! available to the AoT backend; the linker can dead-strip later, but this pass
//! never prunes language-level definitions.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};

use pon_ir::lower::{DynamicSink, LowerError, SourceSpan, scan_dynamic_sinks_source};
use pon_ir::{InstKind, Module, NameId, lower_source};

/// Options that affect AoT reachability policy.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReachabilityOptions {
    /// Allow statically-reached dynamic code sinks and mark the build as needing
    /// the optional runtime/JIT dynamic-code path.
    pub allow_dynamic: bool,
}

/// One Python source file selected for AoT compilation.
#[derive(Clone, Debug, PartialEq)]
pub struct CompileUnit {
    /// Canonical path to the source file.
    pub path: PathBuf,
    /// Fully-qualified dotted import name; `__main__` for the entry module.
    pub module_name: String,
    /// True when this unit is a package `__init__.py`.
    pub is_package: bool,
    /// Source text used for dynamic diagnostics and lowering.
    pub source: String,
    /// Tier-0 boxed IR for this module.
    pub module: Module,
    /// Direct dynamic-code calls found in this module.
    pub dynamic_sinks: Vec<DynamicSink>,
    /// Static imports observed in the lowered IR and their resolver outcomes.
    pub imports: Vec<ImportEdge>,
    /// Dynamic import APIs observed in this module. These are intentionally not
    /// followed by the static module closure.
    pub dynamic_imports: Vec<DynamicImportEdge>,
}

/// Full reachability result consumed by the AoT build pipeline.
#[derive(Clone, Debug, PartialEq)]
pub struct ReachabilityReport {
    /// Modules in deterministic discovery order. The entry module is first.
    pub units: Vec<CompileUnit>,
    /// True when `--allow-dynamic` permitted at least one reached dynamic sink.
    pub requires_dynamic_runtime: bool,
}

/// A module-name edge discovered from `ImportName` IR.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaticImport {
    /// Dotted module target as written; empty for `from . import name` forms.
    pub module: String,
    /// Relative-import level (`from ..x import y` is level 2); zero = absolute.
    pub level: u32,
}

/// A resolved or recorded static import edge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportEdge {
    /// The import target that was scanned.
    pub import: StaticImport,
    /// Resolver decision for that target.
    pub resolution: ImportResolution,
}

/// Dynamic import API use observed in lowered IR.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DynamicImportEdge {
    /// User-facing description of the dynamic import edge.
    pub description: String,
}

/// Static import resolution outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportResolution {
    /// Resolved to a pure-Python file that should join the closure.
    Static(PathBuf),
    /// Known dynamic edge; not embedded unless later `--allow-dynamic` support
    /// ships source and a JIT path.
    Dynamic(String),
    /// Statically named but unsupported/unavailable to the AoT closure.
    Unsupported(String),
    /// No source file was found on the configured static import path.
    NotFound,
}

/// Resolver used by reachability; implementations must not execute Python code.
pub trait StaticImportResolver {
    /// Resolve `import` as seen from `importer`.
    fn resolve(&self, importer: &Path, import: &StaticImport) -> Result<ImportResolution, ReachabilityError>;
}

/// Filesystem resolver for sibling modules and explicit search roots.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PathImportResolver {
    search_roots: Vec<PathBuf>,
}

impl PathImportResolver {
    /// Create a resolver with additional import roots searched after the
    /// importing module's own directory.
    #[must_use]
    pub fn new(search_roots: Vec<PathBuf>) -> Self {
        Self { search_roots }
    }

    fn candidate_roots(&self, importer: &Path) -> Vec<PathBuf> {
        let mut roots = Vec::with_capacity(self.search_roots.len() + 1);
        if let Some(parent) = importer.parent() {
            roots.push(parent.to_owned());
        }
        roots.extend(self.search_roots.iter().cloned());
        roots
    }
}

impl StaticImportResolver for PathImportResolver {
    fn resolve(&self, importer: &Path, import: &StaticImport) -> Result<ImportResolution, ReachabilityError> {
        if import.module.is_empty() {
            return Ok(ImportResolution::Dynamic("empty import target".to_owned()));
        }

        for root in self.candidate_roots(importer) {
            if let Some(path) = resolve_module_under_root(&root, &import.module) {
                return Ok(ImportResolution::Static(path));
            }
        }

        Ok(ImportResolution::NotFound)
    }
}

/// Compute the default AoT module closure with dynamic-code sinks rejected.
pub fn module_closure(entry: &Path, import_resolver: &dyn StaticImportResolver) -> Result<Vec<CompileUnit>, ReachabilityError> {
    module_closure_with_options(entry, import_resolver, &ReachabilityOptions::default()).map(|report| report.units)
}

/// Compute the AoT module closure and apply dynamic-code policy.
pub fn module_closure_with_options(
    entry: &Path,
    import_resolver: &dyn StaticImportResolver,
    options: &ReachabilityOptions,
) -> Result<ReachabilityReport, ReachabilityError> {
    let entry = canonicalize_existing(entry)?;
    let mut worklist = VecDeque::from([PendingUnit {
        path: entry,
        name: "__main__".to_owned(),
        is_package: false,
    }]);
    let mut seen = BTreeSet::new();
    let mut assigned = BTreeMap::new();
    let mut units = Vec::new();
    let mut requires_dynamic_runtime = false;

    while let Some(pending) = worklist.pop_front() {
        let path = canonicalize_existing(&pending.path)?;
        if !seen.insert((path.clone(), pending.name.clone())) {
            continue;
        }

        let source = fs::read_to_string(&path).map_err(|err| ReachabilityError::Io {
            path: path.clone(),
            message: err.to_string(),
        })?;

        let dynamic_sinks = scan_dynamic_sinks_source(&source).map_err(|error| ReachabilityError::Lower {
            path: path.clone(),
            error,
        })?;
        if !dynamic_sinks.is_empty() {
            if options.allow_dynamic {
                requires_dynamic_runtime = true;
            } else {
                return Err(ReachabilityError::DynamicCode {
                    path: path.clone(),
                    location: source_location(&source, dynamic_sinks[0].span),
                    sink: dynamic_sinks[0].clone(),
                });
            }
        }

        let module = lower_source(&source).map_err(|error| ReachabilityError::Lower {
            path: path.clone(),
            error,
        })?;
        let imports = static_imports(&module);
        let dynamic_imports = dynamic_import_edges(&module);
        let mut import_edges = Vec::with_capacity(imports.len());
        let package_root = unit_package_root(&path, &pending.name, pending.is_package);

        for import in imports {
            let absolute = if import.level == 0 {
                Some(import.module.clone())
            } else {
                relative_import_target(&pending.name, pending.is_package, &import)
            };
            let resolution = match absolute.as_deref() {
                None => ImportResolution::Unsupported(
                    "relative import outside a package is resolved (and refused) at runtime".to_owned(),
                ),
                Some(_) if import.level == 0 => import_resolver.resolve(&path, &import)?,
                Some(absolute) => package_root
                    .as_deref()
                    .and_then(|root| resolve_module_under_root(root, absolute))
                    .map_or(ImportResolution::NotFound, ImportResolution::Static),
            };
            if let (Some(absolute), ImportResolution::Static(import_path)) = (absolute.as_deref(), &resolution) {
                let import_path = canonicalize_existing(import_path)?;
                enqueue_with_ancestor_packages(&mut worklist, &mut assigned, absolute, &import_path);
            }
            import_edges.push(ImportEdge { import, resolution });
        }

        units.push(CompileUnit {
            path,
            module_name: pending.name,
            is_package: pending.is_package,
            source,
            module,
            dynamic_sinks,
            imports: import_edges,
            dynamic_imports,
        });
    }

    Ok(ReachabilityReport {
        units,
        requires_dynamic_runtime,
    })
}

/// Extract statically named imports from lowered IR.
#[must_use]
pub fn static_imports(module: &Module) -> Vec<StaticImport> {
    let mut imports = Vec::new();
    let mut seen = BTreeSet::new();

    for function in &module.functions {
        for block in &function.blocks {
            for inst in &block.insts {
                let InstKind::ImportName { name, fromlist, level } = &inst.kind else {
                    continue;
                };
                let Some(module_name) = name_string(module, *name) else {
                    continue;
                };
                push_import(&mut imports, &mut seen, module_name.to_owned(), *level);

                for member in fromlist {
                    let Some(member_name) = name_string(module, *member) else {
                        continue;
                    };
                    if member_name == "*" {
                        continue;
                    }
                    let candidate = if module_name.is_empty() {
                        member_name.to_owned()
                    } else {
                        format!("{module_name}.{member_name}")
                    };
                    push_import(&mut imports, &mut seen, candidate, *level);
                }
            }
        }
    }

    imports
}

/// Extract dynamic import API calls that the static module closure cannot follow.
#[must_use]
pub fn dynamic_import_edges(module: &Module) -> Vec<DynamicImportEdge> {
    let mut edges = Vec::new();
    let mut seen = BTreeSet::new();

    for function in &module.functions {
        for block in &function.blocks {
            let mut value_names = Vec::new();
            let mut value_attrs = Vec::new();

            for inst in &block.insts {
                match &inst.kind {
                    InstKind::LoadBuiltin(name) | InstKind::LoadGlobal(name) | InstKind::LoadName(name) => {
                        if let Some(name) = name_string(module, *name) {
                            value_names.push((inst.result, name.to_owned()));
                        }
                    }
                    InstKind::LoadAttr { name, .. } => {
                        if let Some(name) = name_string(module, *name) {
                            value_attrs.push((inst.result, name.to_owned()));
                        }
                    }
                    InstKind::Call { callee, .. } | InstKind::CallEx { callee, .. } => {
                        if value_names.contains(&(*callee, "__import__".to_owned())) {
                            push_dynamic_import(&mut edges, &mut seen, "__import__(...)");
                        }
                        if value_attrs.contains(&(*callee, "import_module".to_owned())) {
                            push_dynamic_import(&mut edges, &mut seen, "importlib.import_module(...)");
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    edges
}

fn push_dynamic_import(edges: &mut Vec<DynamicImportEdge>, seen: &mut BTreeSet<String>, description: &str) {
    if seen.insert(description.to_owned()) {
        edges.push(DynamicImportEdge {
            description: description.to_owned(),
        });
    }
}

fn push_import(imports: &mut Vec<StaticImport>, seen: &mut BTreeSet<(String, u32)>, module: String, level: u32) {
    if seen.insert((module.clone(), level)) {
        imports.push(StaticImport { module, level });
    }
}

/// A discovered-but-not-yet-lowered module in the reachability worklist.
struct PendingUnit {
    path: PathBuf,
    name: String,
    is_package: bool,
}

/// Compute the absolute dotted target of a relative import as seen from
/// `unit_name`. `None` means the import has no statically-known parent package
/// (entry module, top-level module, or a level walking past the top); the
/// runtime raises the matching `ImportError` when the statement executes.
fn relative_import_target(unit_name: &str, unit_is_package: bool, import: &StaticImport) -> Option<String> {
    let base = if unit_is_package {
        unit_name
    } else {
        unit_name.rsplit_once('.').map(|(parent, _)| parent)?
    };
    let mut parts = base.split('.').collect::<Vec<_>>();
    let strip = import.level.saturating_sub(1) as usize;
    if strip >= parts.len() {
        return None;
    }
    parts.truncate(parts.len() - strip);
    if !import.module.is_empty() {
        parts.extend(import.module.split('.'));
    }
    Some(parts.join("."))
}

/// Directory against which this unit's absolute dotted imports resolve: the
/// unit's own directory with one component removed per dotted-name component
/// below the top-level package.
fn unit_package_root(path: &Path, name: &str, is_package: bool) -> Option<PathBuf> {
    let mut root = path.parent()?.to_path_buf();
    let pops = name.matches('.').count() + usize::from(is_package);
    for _ in 0..pops {
        root = root.parent()?.to_path_buf();
    }
    Some(root)
}

/// Queue a statically resolved module plus every ancestor package
/// `__init__.py` on the filesystem path to it: importing `a.b.c` at runtime
/// imports `a` and `a.b` first, so the closure must embed them too.
fn enqueue_with_ancestor_packages(
    worklist: &mut VecDeque<PendingUnit>,
    assigned: &mut BTreeMap<String, PathBuf>,
    absolute: &str,
    import_path: &Path,
) {
    let is_package = import_path.file_name().is_some_and(|name| name == "__init__.py");
    enqueue_module(worklist, assigned, absolute, import_path, is_package);

    let mut dir = import_path.parent();
    if is_package {
        dir = dir.and_then(Path::parent);
    }
    let mut name = absolute;
    while let Some((parent_name, _)) = name.rsplit_once('.') {
        let Some(parent_dir) = dir else {
            break;
        };
        let init = parent_dir.join("__init__.py");
        if init.is_file() {
            enqueue_module(worklist, assigned, parent_name, &init, true);
        }
        dir = parent_dir.parent();
        name = parent_name;
    }
}

/// Queue one module unit unless its dotted name is already owned by an earlier
/// resolution (first wins, mirroring ordered import roots at runtime).
fn enqueue_module(
    worklist: &mut VecDeque<PendingUnit>,
    assigned: &mut BTreeMap<String, PathBuf>,
    name: &str,
    path: &Path,
    is_package: bool,
) {
    if assigned.contains_key(name) {
        return;
    }
    assigned.insert(name.to_owned(), path.to_owned());
    worklist.push_back(PendingUnit {
        path: path.to_owned(),
        name: name.to_owned(),
        is_package,
    });
}

fn name_string(module: &Module, name: NameId) -> Option<&str> {
    module.names.get(name.0 as usize).map(String::as_str)
}

fn resolve_module_under_root(root: &Path, module: &str) -> Option<PathBuf> {
    let mut rel = PathBuf::new();
    for part in module.split('.') {
        if !is_python_identifier(part) {
            return None;
        }
        rel.push(part);
    }

    let file = root.join(rel.with_extension("py"));
    if file.is_file() {
        return canonicalize_existing(&file).ok();
    }

    let package = root.join(rel).join("__init__.py");
    if package.is_file() {
        return canonicalize_existing(&package).ok();
    }

    None
}

fn is_python_identifier(part: &str) -> bool {
    let mut chars = part.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic()) && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn canonicalize_existing(path: &Path) -> Result<PathBuf, ReachabilityError> {
    fs::canonicalize(path).map_err(|err| ReachabilityError::Io {
        path: path.to_owned(),
        message: err.to_string(),
    })
}

fn source_location(source: &str, span: SourceSpan) -> SourceLocation {
    let target = span.start as usize;
    let mut line = 1usize;
    let mut column = 1usize;

    for (index, byte) in source.bytes().enumerate() {
        if index >= target {
            break;
        }
        if byte == b'\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }

    SourceLocation { line, column }
}

/// One-based line and byte-column for a source span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceLocation {
    /// One-based line number.
    pub line: usize,
    /// One-based byte column.
    pub column: usize,
}

/// Reachability failure with enough context for build diagnostics.
#[derive(Debug)]
pub enum ReachabilityError {
    /// File IO failed.
    Io { path: PathBuf, message: String },
    /// Parsing or lowering failed before a module could enter the closure.
    Lower { path: PathBuf, error: LowerError },
    /// Direct dynamic-code use was reached while `allow_dynamic` was false.
    DynamicCode {
        path: PathBuf,
        location: SourceLocation,
        sink: DynamicSink,
    },
}

impl Display for ReachabilityError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, message } => write!(f, "failed to read `{}`: {message}", path.display()),
            Self::Lower { path, error } => write!(f, "failed to lower `{}` for AoT reachability: {error}", path.display()),
            Self::DynamicCode { path, location, sink } => write!(
                f,
                "`{}` reached statically at {}:{}:{} is unsupported in AoT builds; rebuild with --allow-dynamic to embed dynamic-code support",
                sink.kind.as_str(),
                path.display(),
                location.line,
                location.column
            ),
        }
    }
}

impl Error for ReachabilityError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn closes_over_two_file_static_import() {
        let root = temp_root("closure");
        fs::create_dir_all(&root).expect("temp root should be creatable");
        let app = root.join("app.py");
        let util = root.join("util.py");
        fs::write(&app, "import util\nprint(util.value)\n").expect("app should be writable");
        fs::write(&util, "value = 2\n").expect("util should be writable");

        let resolver = PathImportResolver::default();
        let units = module_closure(&app, &resolver).expect("static sibling import should close");

        assert_eq!(units.len(), 2);
        assert_eq!(units[0].path, canonicalize_existing(&app).expect("app should exist"));
        assert_eq!(units[1].path, canonicalize_existing(&util).expect("util should exist"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_dynamic_sink_by_default_and_allows_with_option() {
        let root = temp_root("dynamic");
        fs::create_dir_all(&root).expect("temp root should be creatable");
        let app = root.join("app.py");
        fs::write(&app, "print(eval('1 + 1'))\n").expect("app should be writable");

        let resolver = PathImportResolver::default();
        let err = module_closure(&app, &resolver).expect_err("eval should be rejected by default");
        assert!(matches!(err, ReachabilityError::DynamicCode { .. }));

        let report = module_closure_with_options(
            &app,
            &resolver,
            &ReachabilityOptions { allow_dynamic: true },
        )
        .expect("allow_dynamic should permit closure construction");
        assert!(report.requires_dynamic_runtime);
        assert_eq!(report.units.len(), 1);
        assert_eq!(report.units[0].dynamic_sinks.len(), 1);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_each_dynamic_code_builtin_with_construct_and_location() {
        let root = temp_root("dynamic-message");
        fs::create_dir_all(&root).expect("temp root should be creatable");
        let resolver = PathImportResolver::default();

        for (file_name, source, construct, line, column) in [
            ("eval_sink.py", "print(eval('1 + 1'))\n", "eval", 1, 7),
            ("exec_sink.py", "x = 0\nexec('x = 1')\n", "exec", 2, 1),
            ("compile_sink.py", "code = compile('1', '<dyn>', 'eval')\n", "compile", 1, 8),
        ] {
            let path = root.join(file_name);
            fs::write(&path, source).expect("dynamic sink fixture should be writable");
            let err = module_closure(&path, &resolver).expect_err("dynamic sink should be rejected");
            let message = err.to_string();
            assert!(
                message.contains(&format!("`{construct}` reached statically")),
                "message should name construct {construct}: {message}"
            );
            assert!(
                message.contains(&format!(":{line}:{column}")),
                "message should include source location {line}:{column}: {message}"
            );
        }

        let _ = fs::remove_dir_all(root);
    }


    #[test]
    fn records_dynamic_import_api_without_following_it() {
        let root = temp_root("dynamic-import");
        fs::create_dir_all(&root).expect("temp root should be creatable");
        let app = root.join("app.py");
        fs::write(
            &app,
            "import importlib\nname = 'util'\nimportlib.import_module(name)\n",
        )
        .expect("app should be writable");

        let resolver = PathImportResolver::default();
        let report = module_closure_with_options(&app, &resolver, &ReachabilityOptions::default())
            .expect("dynamic import API should not expand the static closure");

        assert_eq!(report.units.len(), 1);
        assert_eq!(report.units[0].dynamic_imports.len(), 1);
        assert_eq!(report.units[0].dynamic_imports[0].description, "importlib.import_module(...)");

        let _ = fs::remove_dir_all(root);
    }
    fn temp_root(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("pon-aot-reachable-{label}-{nanos}"))
    }
}
