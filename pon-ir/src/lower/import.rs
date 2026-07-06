use ruff_python_ast::Identifier;

use super::*;

pub(super) fn lower_import_stmt(
	driver: &mut LoweringDriver,
	scope: &mut BodyScope,
	stmt: &ruff_python_ast::StmtImport,
) -> Result<(), LowerError> {
	for alias in &stmt.names {
		let module_name = alias.name.as_str();
		if module_name.starts_with('.') {
			return unsupported_at(
				"relative import statement",
				span_bounds(alias.range.start().to_u32(), alias.range.end().to_u32()),
			);
		}

		let import_name = driver.names.intern(module_name)?;
		let module = scope.emit(InstKind::ImportName {
			name:     import_name,
			fromlist: Vec::new(),
			level:    0,
		})?;

		let binding = import_binding_name(&alias.name, alias.asname.as_ref());
		// `import a.b` binds the root module that a fromlist-empty ImportName
		// returns, but `import a.b as x` binds the LEAF submodule.  Mirror
		// CPython's compiler_import_as (bpo-30024): walk one ImportFrom per
		// dotted component after the first, so the runtime's package-child
		// fallback also serves partially initialized parents.
		let mut value = module;
		if alias.asname.is_some() {
			for attr in module_name.split('.').skip(1) {
				let attr_id = driver.names.intern(attr)?;
				value = scope.emit(InstKind::ImportFrom { module: value, name: attr_id })?;
			}
		}
		store_import_binding(driver, scope, &binding, value)?;
	}

	Ok(())
}

pub(super) fn lower_import_from_stmt(
	driver: &mut LoweringDriver,
	scope: &mut BodyScope,
	stmt: &ruff_python_ast::StmtImportFrom,
) -> Result<(), LowerError> {
	if stmt.level == 0 && stmt.module.is_none() {
		return unsupported_at(
			"relative import-from statement",
			span_bounds(stmt.range.start().to_u32(), stmt.range.end().to_u32()),
		);
	}

	let module_name = stmt
		.module
		.as_ref()
		.map(Identifier::as_str)
		.unwrap_or_default();
	let import_name = driver.names.intern(module_name)?;
	let fromlist = stmt
		.names
		.iter()
		.map(|alias| driver.names.intern(alias.name.as_str()))
		.collect::<Result<Vec<_>, _>>()?;
	let module =
		scope.emit(InstKind::ImportName { name: import_name, fromlist, level: stmt.level })?;

	for alias in &stmt.names {
		if alias.name.as_str() == "*" {
			scope.emit(InstKind::ImportStar { module })?;
			continue;
		}

		let attr_name = driver.names.intern(alias.name.as_str())?;
		let value = scope.emit(InstKind::ImportFrom { module, name: attr_name })?;
		let binding = import_binding_name(&alias.name, alias.asname.as_ref());
		store_import_binding(driver, scope, &binding, value)?;
	}

	Ok(())
}

fn store_import_binding(
	driver: &mut LoweringDriver,
	scope: &mut BodyScope,
	binding: &str,
	value: Value,
) -> Result<(), LowerError> {
	let name_id = driver.names.intern(binding)?;
	if scope.is_global_name(binding) {
		scope.emit(InstKind::StoreGlobal(name_id, value))?;
	} else if let Some(slot) = scope.local_slot(binding) {
		scope.emit(InstKind::StoreLocal(slot, value))?;
	} else {
		scope.emit(InstKind::StoreName(name_id, value))?;
	}
	Ok(())
}

fn import_binding_name(name: &Identifier, asname: Option<&Identifier>) -> String {
	if let Some(asname) = asname {
		return asname.as_str().to_owned();
	}
	name
		.as_str()
		.split('.')
		.next()
		.unwrap_or(name.as_str())
		.to_owned()
}

// Scope analysis has already validated declaration legality and applied the
// declarations to name classification. No runtime IR is emitted for either
// statement.
pub(super) fn lower_global(_stmt: &ruff_python_ast::StmtGlobal) -> Result<(), LowerError> {
	Ok(())
}

pub(super) fn lower_nonlocal(_stmt: &ruff_python_ast::StmtNonlocal) -> Result<(), LowerError> {
	Ok(())
}

pub(crate) fn is_known_builtin_name(name: &str) -> bool {
	matches!(
		name,
		"abs"
			| "all"
			| "any"
			| "bool"
			| "bytes"
			| "callable"
			| "chr"
			| "classmethod"
			| "complex"
			| "dict"
			| "dir"
			| "divmod"
			| "enumerate"
			| "filter"
			| "format"
			| "float"
			| "getattr"
			| "globals"
			| "hasattr"
			| "Ellipsis"
			| "hash"
			| "int"
			| "isinstance"
			| "issubclass"
			| "iter"
			| "len"
			| "list"
			| "locals"
			| "map"
			| "max"
			| "min"
			| "next"
			| "NotImplemented"
			| "object"
			| "pow"
			| "print"
			| "property"
			| "range"
			| "repr"
			| "round"
			| "set"
			| "setattr"
			| "slice"
			| "sorted"
			| "staticmethod"
			| "str"
			| "sum"
			| "super"
			| "tuple"
			| "type"
			| "zip"
	)
}
