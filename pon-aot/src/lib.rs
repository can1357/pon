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
/// Phase C WS-C1 wires the backend retarget and object/link pipeline. Whole-program
/// reachability and dynamic-code policy are supplied by later Phase C workstreams;
/// until then this compiles the entry module itself through the existing boxed
/// baseline lowering and exports a zero-argument `pon_module_main` wrapper.
pub fn build(entry_path: &Path, opts: &BuildOptions) -> anyhow::Result<PathBuf> {

    let ir_module = if opts.opt {
        typed_ir_module(entry_path, opts.allow_dynamic).context("failed to seed typed AoT IR")?
    } else {
        let resolver = reachable::PathImportResolver::default();
        let reachability = reachable::module_closure_with_options(
            entry_path,
            &resolver,
            &reachable::ReachabilityOptions {
                allow_dynamic: opts.allow_dynamic,
            },
        )
        .context("failed to compute AoT reachability")?;
        reachability
            .units
            .first()
            .context("AoT reachability produced no entry module")?
            .module
            .clone()
    };

    let runtime_names = aot_runtime_name_prefix(&ir_module.names);

    let triple = opts.target.clone().unwrap_or_else(Triple::host);
    let isa = isa::build_isa(opts.target.clone());
    let mut module = object_module::new_object_module(isa, "pon_module")?;
    let mut ctx = module.make_context();
    let mut fctx = FunctionBuilderContext::new();

    let func_ids = if opts.opt {
        pon_codegen::compile_optimized_ir_module(
            &mut module,
            &ir_module,
            pon_codegen::CompileMode::Aot,
            &mut ctx,
            &mut fctx,
        )
        .context("failed to compile optimized AoT module body")?
    } else {
        pon_codegen::compile_ir_module(
            &mut module,
            &ir_module,
            pon_codegen::CompileMode::Aot,
            &mut ctx,
            &mut fctx,
        )
        .context("failed to compile AoT module body")?
    };
    let module_body_id = func_ids
        .get(ir_module.main.0 as usize)
        .copied()
        .context("AoT IR main function id missing")?;
    entry::define_aot_name_initializer(&mut module, &runtime_names)
        .context("failed to emit AoT runtime name initializer")?;

    entry::define_module_main_wrapper(&mut module, module_body_id)
        .context("failed to emit AoT module main wrapper")?;
    entry::define_main_trampoline(&mut module).context("failed to emit AoT main trampoline")?;

    let object_path = object_path_for(&opts.out_path);
    object_module::finish_to_object_file(module, &triple, &object_path)
        .with_context(|| format!("failed to emit object file {}", object_path.display()))?;

    let runtime_archive = link::locate_runtime_archive()?;
    link::link_executable(&object_path, &runtime_archive, &opts.out_path, &triple)?;

    Ok(opts.out_path.clone())
}

fn aot_runtime_name_prefix(module_names: &[String]) -> Vec<String> {
    let max_name_id = module_names
        .iter()
        .map(|name| pon_runtime::intern::intern(name))
        .max();

    let Some(max_name_id) = max_name_id else {
        return Vec::new();
    };

    (0..=max_name_id)
        .map(|id| {
            pon_runtime::intern::resolve(id)
                .expect("AoT name ids interned during this build should resolve")
        })
        .collect()
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

    let mut module = pon_ir::lower_module(&lowerable)
        .with_context(|| format!("failed to lower `{}` for typed AoT", entry_path.display()))?;
    pon_codegen::infer_module_types(&mut module, &annotations);
    Ok(module)
}

fn object_path_for(out_path: &Path) -> PathBuf {
    out_path.with_extension("o")
}
