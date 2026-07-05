//! Object-module construction and object-file emission for AoT builds.

use std::path::Path;

use anyhow::Context;
use cranelift_codegen::isa::OwnedTargetIsa;
use cranelift_module::{ModuleResult, default_libcall_names};
use cranelift_object::{ObjectBuilder, ObjectModule};
use target_lexicon::{BinaryFormat, Triple};

use crate::buildver::stamp_macho_build_version;

/// Create a Cranelift object module using Pon's AoT ISA.
pub fn new_object_module(isa: OwnedTargetIsa, name: &str) -> ModuleResult<ObjectModule> {
	let per_function_sections = isa.triple().binary_format != BinaryFormat::Coff;
	let mut builder = ObjectBuilder::new(isa, name, default_libcall_names())?;
	builder.per_function_section(per_function_sections);
	Ok(ObjectModule::new(builder))
}

/// Finish `module`, stamp any platform object metadata, and write it to `path`.
pub fn finish_to_object_file(
	module: ObjectModule,
	triple: &Triple,
	path: &Path,
) -> anyhow::Result<()> {
	let mut product = module.finish();
	stamp_macho_build_version(&mut product.object, triple);
	let bytes = product
		.emit()
		.context("failed to serialize Cranelift object")?;
	if let Some(parent) = path
		.parent()
		.filter(|parent| !parent.as_os_str().is_empty())
	{
		std::fs::create_dir_all(parent)
			.with_context(|| format!("failed to create {}", parent.display()))?;
	}
	std::fs::write(path, bytes).with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
	use cranelift_codegen::ir::AbiParam;
	use cranelift_module::{Linkage, Module};

	use super::*;

	#[test]
	fn module_main_export_is_zero_arg_wrapper_not_boxed_body() {
		let isa = crate::isa::build_isa(None);
		let mut module = new_object_module(isa, "aot_module_main_abi").expect("object module");
		let ptr_ty = module.target_config().pointer_type();

		let mut body_sig = module.make_signature();
		body_sig.params.push(AbiParam::new(ptr_ty));
		body_sig.params.push(AbiParam::new(ptr_ty));
		body_sig.returns.push(AbiParam::new(ptr_ty));
		let body_id = module
			.declare_function("__pon_module_body_test", Linkage::Local, &body_sig)
			.expect("body declaration");

		let wrapper_id = crate::entry::define_module_main_wrapper(&mut module, body_id)
			.expect("module-main wrapper");
		let declarations = module.declarations();
		let wrapper_decl = declarations
			.get_functions()
			.find_map(|(id, decl)| (id == wrapper_id).then_some(decl))
			.expect("wrapper declaration");
		let body_decl = declarations
			.get_functions()
			.find_map(|(id, decl)| (id == body_id).then_some(decl))
			.expect("body declaration");

		assert_eq!(wrapper_decl.name.as_deref(), Some(pon_codegen::AOT_MODULE_MAIN));
		assert_eq!(wrapper_decl.linkage, Linkage::Export);
		assert!(wrapper_decl.signature.params.is_empty());
		assert_eq!(wrapper_decl.signature.returns.len(), 1);
		assert_eq!(wrapper_decl.signature.returns[0].value_type, ptr_ty);

		assert_eq!(body_decl.name.as_deref(), Some("__pon_module_body_test"));
		assert_eq!(body_decl.linkage, Linkage::Local);
		assert_eq!(body_decl.signature.params.len(), 2);
		assert_eq!(body_decl.signature.returns.len(), 1);
	}
}
