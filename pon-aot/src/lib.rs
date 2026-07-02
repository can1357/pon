#![doc = "Ahead-of-time object emission and linking for Pon."]

use std::{fs, path::{Path, PathBuf}};

use anyhow::{Context, bail};
use cranelift_frontend::FunctionBuilderContext;
use cranelift_module::Module as _;
use target_lexicon::Triple;

pub mod buildver;
pub mod entry;
pub mod isa;
pub mod link;
pub mod object_module;
pub mod reachable;

/// Options for building a Pon source file into a native executable.
#[derive(Clone, Debug)]
pub struct BuildOptions {
    /// Final executable path.
    pub out_path: PathBuf,
    /// Permit runtime dynamic-code support in the produced executable.
    pub allow_dynamic: bool,
    /// Enable typed AoT optimizations once Phase D wires the typed tier.
    pub opt: bool,
    /// Target triple to compile for. `None` targets the current host.
    pub target: Option<Triple>,
}

/// Build `entry_path` into a native executable and return the output path.
///
/// The boxed baseline path compiles the whole static module closure: the entry
/// module exports the zero-argument `pon_module_main` wrapper, and every other
/// reachability unit becomes its own object file whose body the generated
/// `pon_aot_init_modules` registrar announces to the runtime import machinery.
/// The typed `--opt` path still compiles the entry module alone.
pub fn build(entry_path: &Path, opts: &BuildOptions) -> anyhow::Result<PathBuf> {
    let (entry_module, embedded_units) = if opts.opt {
        let module = typed_ir_module(entry_path, opts.allow_dynamic).context("failed to seed typed AoT IR")?;
        (module, Vec::new())
    } else {
        let resolver = reachable::PathImportResolver::with_runtime_import_order(
            pon_runtime::import::source_search_roots(),
            pon_runtime::import::import_shadowed_from_source,
        );
        let reachability = reachable::module_closure_with_options(
            entry_path,
            &resolver,
            &reachable::ReachabilityOptions {
                allow_dynamic: opts.allow_dynamic,
                ..Default::default()
            },
        )
        .context("failed to compute AoT reachability")?;
        if std::env::var_os("PON_AOT_DEBUG_CLOSURE").is_some() {
            for unit in &reachability.units {
                eprintln!("unit: {} <- {}", unit.module_name, unit.path.display());
                for edge in &unit.imports {
                    eprintln!("    import {:?} -> {:?}", edge.import.module, edge.resolution);
                }
            }
            for skip in &reachability.skipped {
                eprintln!("skip: {} ({})", skip.module_name, skip.reason);
            }
        }
        let mut units = reachability.units.into_iter();
        let entry_unit = units.next().context("AoT reachability produced no entry module")?;
        (entry_unit.module, units.collect())
    };

    let triple = opts.target.clone().unwrap_or_else(Triple::host);
    let mut object_paths = vec![object_path_for(&opts.out_path)];
    let mut embedded_specs = Vec::with_capacity(embedded_units.len());

    // Embedded (non-entry) reachability units: one object file per module, each
    // exporting a unique zero-argument body wrapper that the entry object's
    // registrar hands to the runtime. These compile before the interner
    // snapshot below so every name id they bake into object data is replayed
    // by the generated initializer.
    for (index, unit) in embedded_units.iter().enumerate() {
        let isa = isa::build_isa(opts.target.clone());
        let mut unit_object = object_module::new_object_module(isa, &format!("pon_unit_{index}"))?;
        let mut unit_ctx = unit_object.make_context();
        let mut unit_fctx = FunctionBuilderContext::new();
        let compiled = pon_codegen::compile_ir_module(
            &mut unit_object,
            &unit.module,
            pon_codegen::CompileMode::Aot,
            &mut unit_ctx,
            &mut unit_fctx,
        );
        let func_ids = match compiled {
            Ok(func_ids) => func_ids,
            // Best-effort units (runtime import roots) may outrun codegen
            // support; dropping one reproduces the no-embedding runtime
            // behavior for that module instead of failing the whole build.
            Err(error) if unit.best_effort => {
                eprintln!("pon: warning: not embedding module `{}`: {error:#}", unit.module_name);
                continue;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to compile embedded module `{}`", unit.module_name));
            }
        };
        let body_id = func_ids
            .get(unit.module.main.0 as usize)
            .copied()
            .with_context(|| format!("embedded module `{}` is missing its main function id", unit.module_name))?;
        let symbol = format!("__pon_embedded_body_{index}");
        entry::define_zero_arg_body_wrapper(&mut unit_object, body_id, &symbol)
            .with_context(|| format!("failed to emit body wrapper for `{}`", unit.module_name))?;
        let unit_object_path = unit_object_path_for(&opts.out_path, index);
        object_module::finish_to_object_file(unit_object, &triple, &unit_object_path)
            .with_context(|| format!("failed to emit object file {}", unit_object_path.display()))?;
        object_paths.push(unit_object_path);
        embedded_specs.push(entry::EmbeddedModuleSpec {
            name: unit.module_name.clone(),
            is_package: unit.is_package,
            symbol,
        });
    }

    let isa = isa::build_isa(opts.target.clone());
    let mut module = object_module::new_object_module(isa, "pon_module")?;
    let mut ctx = module.make_context();
    let mut fctx = FunctionBuilderContext::new();

    let func_ids = if opts.opt {
        pon_codegen::compile_optimized_ir_module(
            &mut module,
            &entry_module,
            pon_codegen::CompileMode::Aot,
            &mut ctx,
            &mut fctx,
        )
        .context("failed to compile optimized AoT module body")?
    } else {
        pon_codegen::compile_ir_module(
            &mut module,
            &entry_module,
            pon_codegen::CompileMode::Aot,
            &mut ctx,
            &mut fctx,
        )
        .context("failed to compile AoT module body")?
    };
    let module_body_id = func_ids
        .get(entry_module.main.0 as usize)
        .copied()
        .context("AoT IR main function id missing")?;
    // Snapshot the build-process interner only after every unit's codegen:
    // object data bakes interned ids for names the IR name table never
    // mentions (parameter specs, keyword-name arrays), and the executable must
    // replay every id it embeds.
    let runtime_names = aot_runtime_name_snapshot(&entry_module.names);
    entry::define_aot_name_initializer(&mut module, &runtime_names)
        .context("failed to emit AoT runtime name initializer")?;

    entry::define_module_main_wrapper(&mut module, module_body_id)
        .context("failed to emit AoT module main wrapper")?;
    entry::define_aot_module_registrar(&mut module, &embedded_specs)
        .context("failed to emit AoT embedded-module registrar")?;
    entry::define_main_trampoline(&mut module).context("failed to emit AoT main trampoline")?;

    object_module::finish_to_object_file(module, &triple, &object_paths[0])
        .with_context(|| format!("failed to emit object file {}", object_paths[0].display()))?;
    debug_assert_eq!(
        pon_runtime::intern::snapshot().len(),
        runtime_names.len(),
        "interner grew after the AoT name snapshot; embedded ids would not replay"
    );

    let runtime_archive = link::locate_runtime_archive()?;
    link::link_executable(&object_paths, &runtime_archive, &opts.out_path, &triple)?;

    Ok(opts.out_path.clone())
}

fn aot_runtime_name_snapshot(module_names: &[String]) -> Vec<String> {
    // Ensure IR-referenced names hold build-process ids even when codegen never
    // touched them, then replay the complete interner so every id baked into
    // object data resolves identically in the produced executable.
    for name in module_names {
        let _ = pon_runtime::intern::intern(name);
    }
    pon_runtime::intern::snapshot()
}

fn typed_ir_module(entry_path: &Path, allow_dynamic: bool) -> anyhow::Result<pon_ir::Module> {
    let source = fs::read_to_string(entry_path)
        .with_context(|| format!("failed to read `{}` for typed AoT", entry_path.display()))?;
    let dynamic_sinks = pon_ir::lower::scan_dynamic_sinks_source(&source)
        .with_context(|| format!("failed to scan `{}` for dynamic-code sinks", entry_path.display()))?;
    if !allow_dynamic {
        if let Some(sink) = dynamic_sinks.first() {
            bail!(
                "`{}` is unsupported in AoT builds; rebuild with --allow-dynamic to embed dynamic-code support",
                sink.kind.as_str()
            );
        }
    }

    let parsed = pon_ir::parse_module_source(&source)
        .with_context(|| format!("failed to parse `{}` for typed AoT", entry_path.display()))?;
    let annotations = pon_codegen::read_module_annotations(&parsed)
        .with_context(|| format!("failed to read type annotations from `{}`", entry_path.display()))?;
    let mut lowerable = parsed.clone();
    pon_codegen::strip_annotations_for_lowering(&mut lowerable);

    let mut module = pon_ir::lower_module(&lowerable, Some(&source))
        .with_context(|| format!("failed to lower `{}` for typed AoT", entry_path.display()))?;
    pon_codegen::infer_module_types(&mut module, &annotations);
    Ok(module)
}

fn object_path_for(out_path: &Path) -> PathBuf {
    out_path.with_extension("o")
}

fn unit_object_path_for(out_path: &Path, index: usize) -> PathBuf {
    out_path.with_extension(format!("unit{index}.o"))
}
