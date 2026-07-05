use ruff_python_ast::InterpolatedStringElement;

use super::*;
use crate::ir::{FStrPart, TStrPart};

const TEMPLATE_LITERAL_CONVERSION: u8 = u8::MAX;

pub(super) fn lower_f_string(
	driver: &mut LoweringDriver,
	scope: &mut BodyScope,
	expr: &ruff_python_ast::ExprFString,
) -> Result<Value, LowerError> {
	// Walk the implicit-concatenation PARTS, not `value.elements()`: ruff's
	// `FStringValue::elements()` yields only the f-string parts' elements,
	// silently dropping plain string literals mixed into the concatenation
	// (`"#define X {" f"{cname};" "%s" "}"` — Cython's ModuleNode builds C
	// macros exactly this way).
	let mut parts = Vec::new();
	for part in expr.value.iter() {
		match part {
			ruff_python_ast::FStringPart::Literal(literal) => {
				let value = scope.emit(InstKind::Const(PyConst::Str(literal.value.to_string())))?;
				parts.push(FStrPart::Interp { value, conversion: 0, format_spec: None });
			},
			ruff_python_ast::FStringPart::FString(fstring) => {
				parts.extend(lower_f_string_parts(driver, scope, fstring.elements.iter())?);
			},
		}
	}
	scope.emit(InstKind::BuildString { parts })
}

pub(super) fn lower_t_string(
	driver: &mut LoweringDriver,
	scope: &mut BodyScope,
	expr: &ruff_python_ast::ExprTString,
) -> Result<Value, LowerError> {
	let parts = lower_t_string_parts(driver, scope, expr.value.elements())?;
	scope.emit(InstKind::BuildTemplate { parts })
}

fn lower_f_string_parts<'a>(
	driver: &mut LoweringDriver,
	scope: &mut BodyScope,
	elements: impl IntoIterator<Item = &'a InterpolatedStringElement>,
) -> Result<Vec<FStrPart>, LowerError> {
	let mut parts = Vec::new();
	for element in elements {
		match element {
			InterpolatedStringElement::Literal(literal) => {
				let value = scope.emit(InstKind::Const(PyConst::Str(literal.value.to_string())))?;
				parts.push(FStrPart::Interp { value, conversion: 0, format_spec: None });
			},
			InterpolatedStringElement::Interpolation(interpolation) => {
				let expression = interpolation_expression_text(driver, &interpolation.expression);
				if let Some(debug) = &interpolation.debug_text {
					let debug_literal = format!("{}{}{}", debug.leading, expression, debug.trailing);
					let value = scope.emit(InstKind::Const(PyConst::Str(debug_literal)))?;
					parts.push(FStrPart::Interp { value, conversion: 0, format_spec: None });
				}
				let value = driver.lower_expr(scope, &interpolation.expression)?;
				let format_spec = interpolation
					.format_spec
					.as_deref()
					.map(|spec| lower_f_string_parts(driver, scope, spec.elements.iter()))
					.transpose()?
					.map(|parts| scope.emit(InstKind::BuildString { parts }))
					.transpose()?;
				let conversion = interpolation.conversion.to_byte().unwrap_or_else(|| {
					if interpolation.debug_text.is_some() && format_spec.is_none() {
						b'r'
					} else {
						0
					}
				});
				parts.push(FStrPart::Interp { value, conversion, format_spec });
			},
		}
	}
	Ok(parts)
}

fn lower_t_string_parts<'a>(
	driver: &mut LoweringDriver,
	scope: &mut BodyScope,
	elements: impl IntoIterator<Item = &'a InterpolatedStringElement>,
) -> Result<Vec<TStrPart>, LowerError> {
	let mut parts = Vec::new();
	for element in elements {
		match element {
			InterpolatedStringElement::Literal(literal) => {
				let value = scope.emit(InstKind::Const(PyConst::Str(literal.value.to_string())))?;
				parts.push(TStrPart::Interp {
					value,
					expression: String::new(),
					conversion: TEMPLATE_LITERAL_CONVERSION,
					format_spec: None,
				});
			},
			InterpolatedStringElement::Interpolation(interpolation) => {
				let expression = interpolation_expression_text(driver, &interpolation.expression);
				if let Some(debug) = &interpolation.debug_text {
					let debug_literal = format!("{}{}{}", debug.leading, expression, debug.trailing);
					let value = scope.emit(InstKind::Const(PyConst::Str(debug_literal)))?;
					parts.push(TStrPart::Interp {
						value,
						expression: String::new(),
						conversion: TEMPLATE_LITERAL_CONVERSION,
						format_spec: None,
					});
				}
				let value = driver.lower_expr(scope, &interpolation.expression)?;
				let format_spec = interpolation
					.format_spec
					.as_deref()
					.map(|spec| lower_f_string_parts(driver, scope, spec.elements.iter()))
					.transpose()?
					.map(|parts| scope.emit(InstKind::BuildString { parts }))
					.transpose()?;
				let conversion = interpolation.conversion.to_byte().unwrap_or_else(|| {
					if interpolation.debug_text.is_some() && format_spec.is_none() {
						b'r'
					} else {
						0
					}
				});
				parts.push(TStrPart::Interp { value, expression, conversion, format_spec });
			},
		}
	}
	Ok(parts)
}

fn interpolation_expression_text(
	driver: &LoweringDriver,
	expression: &ruff_python_ast::Expr,
) -> String {
	driver
		.expr_source(expression)
		.unwrap_or("<expr>")
		.to_owned()
}
