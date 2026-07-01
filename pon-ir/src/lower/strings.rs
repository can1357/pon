use ruff_python_ast::InterpolatedStringElement;

use super::*;
use crate::ir::{FStrPart, TStrPart};

const TEMPLATE_LITERAL_CONVERSION: u8 = u8::MAX;

pub(super) fn lower_f_string(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    expr: &ruff_python_ast::ExprFString,
) -> Result<Value, LowerError> {
    let parts = lower_f_string_parts(driver, scope, expr.value.elements())?;
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
                parts.push(FStrPart::Interp {
                    value,
                    conversion: 0,
                    format_spec: None,
                });
            }
            InterpolatedStringElement::Interpolation(interpolation) => {
                if interpolation.debug_text.is_some() {
                    return unsupported_at(
                        "debug f-string interpolation",
                        span_bounds(interpolation.range.start().to_u32(), interpolation.range.end().to_u32()),
                    );
                }
                let value = driver.lower_expr(scope, &interpolation.expression)?;
                let format_spec = interpolation
                    .format_spec
                    .as_deref()
                    .map(|spec| lower_f_string_parts(driver, scope, spec.elements.iter()))
                    .transpose()?
                    .map(|parts| scope.emit(InstKind::BuildString { parts }))
                    .transpose()?;
                parts.push(FStrPart::Interp {
                    value,
                    conversion: interpolation.conversion.to_byte().unwrap_or(0),
                    format_spec,
                });
            }
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
                    conversion: TEMPLATE_LITERAL_CONVERSION,
                    format_spec: None,
                });
            }
            InterpolatedStringElement::Interpolation(interpolation) => {
                if interpolation.debug_text.is_some() {
                    return unsupported_at(
                        "debug t-string interpolation",
                        span_bounds(interpolation.range.start().to_u32(), interpolation.range.end().to_u32()),
                    );
                }
                let value = driver.lower_expr(scope, &interpolation.expression)?;
                let format_spec = interpolation
                    .format_spec
                    .as_deref()
                    .map(|spec| lower_f_string_parts(driver, scope, spec.elements.iter()))
                    .transpose()?
                    .map(|parts| scope.emit(InstKind::BuildString { parts }))
                    .transpose()?;
                parts.push(TStrPart::Interp {
                    value,
                    conversion: interpolation.conversion.to_byte().unwrap_or(0),
                    format_spec,
                });
            }
        }
    }
    Ok(parts)
}
