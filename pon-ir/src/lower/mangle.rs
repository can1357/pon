//! Compile-time private name mangling (CPython `_Py_Mangle`).
//!
//! Any identifier of the form `__spam` (two leading underscores, at most one
//! trailing underscore) that textually occurs anywhere inside a class body —
//! including method bodies and other scopes nested in it — is rewritten to
//! `_ClassName__spam`, with the class name's leading underscores stripped.
//! The rewrite runs as an AST pre-pass over a lowered module's clone, so both
//! scope analysis and lowering observe the mangled spelling and stay
//! consistent with each other and with explicit references from outside the
//! class (`fut._Future__asyncio_awaited_by` in asyncio's futures.py).
//!
//! Coverage matches the identifiers CPython's symtable/compiler mangle:
//! name loads/stores, attribute names, function/class binding names,
//! parameters, call keywords, `global`/`nonlocal` lists, import aliases, and
//! except-handler capture names. `match` patterns are left unmangled (private
//! names in class-body match statements have no stdlib precedent).

use std::cell::RefCell;

use ruff_python_ast::{
	Expr, Identifier, ModModule, Stmt,
	visitor::transformer::{self, Transformer},
};

/// Rewrites every private identifier in `module` in place.
pub(super) fn mangle_module(module: &mut ModModule) {
	let mangler = Mangler { class: RefCell::new(None) };
	for stmt in &mut module.body {
		mangler.visit_stmt(stmt);
	}
}

/// CPython `_Py_Mangle`: `None` when the name needs no rewrite.
fn mangled(class_name: &str, name: &str) -> Option<String> {
	if !name.starts_with("__") || name.ends_with("__") || name.contains('.') {
		return None;
	}
	let stripped = class_name.trim_start_matches('_');
	if stripped.is_empty() {
		return None;
	}
	Some(format!("_{stripped}{name}"))
}

struct Mangler {
	/// Innermost enclosing class name (mangling context), or `None` outside
	/// any class body.
	class: RefCell<Option<String>>,
}

impl Mangler {
	fn mangle_identifier(&self, identifier: &mut Identifier) {
		let class = self.class.borrow();
		let Some(class_name) = class.as_deref() else {
			return;
		};
		if let Some(rewritten) = mangled(class_name, identifier.id.as_str()) {
			identifier.id = rewritten.into();
		}
	}
}

impl Transformer for Mangler {
	fn visit_stmt(&self, stmt: &mut Stmt) {
		match stmt {
			Stmt::ClassDef(class_def) => {
				// The class NAME binds in the enclosing scope and mangles
				// with the enclosing class context; decorators, type params,
				// and base/keyword arguments evaluate there too.
				self.mangle_identifier(&mut class_def.name);
				for decorator in &mut class_def.decorator_list {
					self.visit_decorator(decorator);
				}
				if let Some(type_params) = class_def.type_params.as_deref_mut() {
					self.visit_type_params(type_params);
				}
				if let Some(arguments) = class_def.arguments.as_deref_mut() {
					self.visit_arguments(arguments);
				}
				// The body mangles with THIS class as context (innermost
				// class wins for nested classes).
				let previous = self
					.class
					.replace(Some(class_def.name.id.as_str().to_owned()));
				for stmt in &mut class_def.body {
					self.visit_stmt(stmt);
				}
				*self.class.borrow_mut() = previous;
			},
			Stmt::FunctionDef(function_def) => {
				// `def __m` binds a mangled name; the body keeps the class
				// context (methods mangle `self.__attr`).
				self.mangle_identifier(&mut function_def.name);
				transformer::walk_stmt(self, stmt);
			},
			Stmt::Global(global) => {
				for name in &mut global.names {
					self.mangle_identifier(name);
				}
			},
			Stmt::Nonlocal(nonlocal) => {
				for name in &mut nonlocal.names {
					self.mangle_identifier(name);
				}
			},
			_ => transformer::walk_stmt(self, stmt),
		}
	}

	fn visit_expr(&self, expr: &mut Expr) {
		match expr {
			Expr::Name(name) => {
				let class = self.class.borrow();
				if let Some(class_name) = class.as_deref() {
					if let Some(rewritten) = mangled(class_name, name.id.as_str()) {
						name.id = rewritten.into();
					}
				}
			},
			Expr::Attribute(attribute) => {
				self.mangle_identifier(&mut attribute.attr);
			},
			_ => {},
		}
		transformer::walk_expr(self, expr);
	}

	fn visit_keyword(&self, keyword: &mut ruff_python_ast::Keyword) {
		if let Some(arg) = keyword.arg.as_mut() {
			self.mangle_identifier(arg);
		}
		transformer::walk_keyword(self, keyword);
	}

	fn visit_parameter(&self, parameter: &mut ruff_python_ast::Parameter) {
		self.mangle_identifier(&mut parameter.name);
		transformer::walk_parameter(self, parameter);
	}

	fn visit_alias(&self, alias: &mut ruff_python_ast::Alias) {
		if let Some(asname) = alias.asname.as_mut() {
			self.mangle_identifier(asname);
		}
	}

	fn visit_except_handler(&self, except_handler: &mut ruff_python_ast::ExceptHandler) {
		let ruff_python_ast::ExceptHandler::ExceptHandler(handler) = except_handler;
		if let Some(name) = handler.name.as_mut() {
			self.mangle_identifier(name);
		}
		transformer::walk_except_handler(self, except_handler);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn mangle_source(source: &str) -> ModModule {
		let mut module = crate::lower::parse_module_source(source).expect("parse");
		mangle_module(&mut module);
		module
	}

	fn class_body(module: &ModModule) -> &[Stmt] {
		match &module.body[0] {
			Stmt::ClassDef(class_def) => &class_def.body,
			other => panic!("expected class, got {other:?}"),
		}
	}

	#[test]
	fn class_body_assignment_and_method_attr_mangle() {
		let module = mangle_source("class C:\n    __x = 1\n    def get(self):\n        return self.__x\n");
		let body = class_body(&module);
		let Stmt::Assign(assign) = &body[0] else {
			panic!("expected assign");
		};
		let Expr::Name(target) = &assign.targets[0] else {
			panic!("expected name target");
		};
		assert_eq!(target.id.as_str(), "_C__x");
		let Stmt::FunctionDef(method) = &body[1] else {
			panic!("expected method");
		};
		let Stmt::Return(ret) = &method.body[0] else {
			panic!("expected return");
		};
		let Some(Expr::Attribute(attribute)) = ret.value.as_deref() else {
			panic!("expected attribute");
		};
		assert_eq!(attribute.attr.id.as_str(), "_C__x");
	}

	#[test]
	fn dunders_module_level_and_underscore_classes_stay_unmangled() {
		// Dunder: both-sided underscores never mangle.
		let module = mangle_source("class C:\n    def __init__(self):\n        self.__x__ = 1\n");
		let Stmt::FunctionDef(init) = &class_body(&module)[0] else {
			panic!("expected function");
		};
		assert_eq!(init.name.id.as_str(), "__init__");
		let Stmt::Assign(assign) = &init.body[0] else {
			panic!("expected assign");
		};
		let Expr::Attribute(attribute) = &assign.targets[0] else {
			panic!("expected attribute");
		};
		assert_eq!(attribute.attr.id.as_str(), "__x__");

		// Module level: no class context, no mangling.
		let module = mangle_source("__private = 1\n");
		let Stmt::Assign(assign) = &module.body[0] else {
			panic!("expected assign");
		};
		let Expr::Name(name) = &assign.targets[0] else {
			panic!("expected name");
		};
		assert_eq!(name.id.as_str(), "__private");

		// All-underscore class names cannot form a prefix.
		let module = mangle_source("class __:\n    __x = 1\n");
		let Stmt::Assign(assign) = &class_body(&module)[0] else {
			panic!("expected assign");
		};
		let Expr::Name(name) = &assign.targets[0] else {
			panic!("expected name");
		};
		assert_eq!(name.id.as_str(), "__x");
	}

	#[test]
	fn nested_class_context_and_leading_underscore_strip() {
		let module = mangle_source(
			"class _Outer:\n    __a = 1\n    class Inner:\n        __b = 2\n",
		);
		let outer_body = class_body(&module);
		let Stmt::Assign(assign) = &outer_body[0] else {
			panic!("expected assign");
		};
		let Expr::Name(name) = &assign.targets[0] else {
			panic!("expected name");
		};
		assert_eq!(name.id.as_str(), "_Outer__a");
		let Stmt::ClassDef(inner) = &outer_body[1] else {
			panic!("expected inner class");
		};
		let Stmt::Assign(assign) = &inner.body[0] else {
			panic!("expected assign");
		};
		let Expr::Name(name) = &assign.targets[0] else {
			panic!("expected name");
		};
		assert_eq!(name.id.as_str(), "_Inner__b");
	}
}
