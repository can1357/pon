//! Match-statement lowering (full 3.14 structural pattern matching).
//!
//! Each case lowers into a chain of tests over the evaluated subject.  A
//! pattern lowers as "test, bind, fall through": predicate instructions branch
//! to the case's fail block on mismatch, capture bindings store eagerly as
//! subpatterns succeed (CPython also leaks partial binds from failed cases),
//! and guards evaluate only after the whole pattern has matched and bound.

use std::collections::BTreeSet;

use crate::ir::CmpOp;
use ruff_python_ast::{Identifier, Pattern, Singleton};

use super::*;

pub(super) fn lower_match(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    stmt: &ruff_python_ast::StmtMatch,
    loop_targets: Option<control::LoopTargets>,
) -> Result<(), LowerError> {
    for case in &stmt.cases {
        validate_case_pattern(&case.pattern)?;
    }

    let subject = driver.lower_expr(scope, &stmt.subject)?;
    let done_block = scope.alloc_block()?;
    for case in &stmt.cases {
        let next_block = scope.alloc_block()?;
        lower_pattern(driver, scope, subject, &case.pattern, next_block)?;
        if let Some(guard) = case.guard.as_deref() {
            let guard = driver.lower_expr(scope, guard)?;
            branch_on(scope, guard, next_block)?;
        }
        driver.lower_stmt_list(scope, &case.body, loop_targets)?;
        scope.jump_if_open(done_block)?;
        scope.switch_to(next_block)?;
    }
    scope.jump_if_open(done_block)?;
    scope.switch_to(done_block)?;
    Ok(())
}

/// Emits a truth test over `cond` and branches to `fail_block` when false.
/// Continues lowering in a fresh block on the true edge.
fn branch_on(scope: &mut BodyScope, cond: Value, fail_block: BlockId) -> Result<(), LowerError> {
    let truth = scope.emit(InstKind::BoolTest { val: cond })?;
    let pass_block = scope.alloc_block()?;
    scope.set_term(Terminator::CondBranch {
        cond: truth,
        then_: pass_block,
        else_: fail_block,
    })?;
    scope.switch_to(pass_block)
}

/// Lowers one pattern against `subject`.
///
/// On return the scope sits in the matched control path with all captures
/// bound; mismatches branch to `fail_block`.
fn lower_pattern(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    subject: Value,
    pattern: &Pattern,
    fail_block: BlockId,
) -> Result<(), LowerError> {
    match pattern {
        Pattern::MatchValue(pattern) => {
            let expected = driver.lower_expr(scope, &pattern.value)?;
            let cond = scope.emit(InstKind::Compare {
                op: CmpOp::Eq,
                lhs: subject,
                rhs: expected,
            })?;
            branch_on(scope, cond, fail_block)
        }
        Pattern::MatchSingleton(pattern) => {
            let expected = scope.emit(InstKind::Const(match pattern.value {
                Singleton::None => PyConst::None,
                Singleton::True => PyConst::Bool(true),
                Singleton::False => PyConst::Bool(false),
            }))?;
            let cond = scope.emit(InstKind::Is {
                lhs: subject,
                rhs: expected,
                negate: false,
            })?;
            branch_on(scope, cond, fail_block)
        }
        Pattern::MatchAs(pattern) => {
            if let Some(nested) = pattern.pattern.as_deref() {
                lower_pattern(driver, scope, subject, nested, fail_block)?;
            }
            if let Some(name) = pattern.name.as_ref() {
                store_capture(driver, scope, name, subject)?;
            }
            Ok(())
        }
        Pattern::MatchOr(pattern) => lower_or_pattern(driver, scope, subject, pattern, fail_block),
        Pattern::MatchSequence(pattern) => {
            lower_sequence_pattern(driver, scope, subject, &pattern.patterns, fail_block)
        }
        Pattern::MatchMapping(pattern) => lower_mapping_pattern(driver, scope, subject, pattern, fail_block),
        Pattern::MatchClass(pattern) => lower_class_pattern(driver, scope, subject, pattern, fail_block),
        Pattern::MatchStar(_) => Err(LowerError::internal(
            "star pattern outside sequence-pattern lowering",
        )),
    }
}

fn store_capture(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    name: &Identifier,
    value: Value,
) -> Result<(), LowerError> {
    driver.store_name_value(scope, name.as_str(), value)
}

fn is_wildcard(pattern: &Pattern) -> bool {
    matches!(pattern, Pattern::MatchAs(as_pattern) if as_pattern.pattern.is_none() && as_pattern.name.is_none())
}

fn lower_or_pattern(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    subject: Value,
    pattern: &ruff_python_ast::PatternMatchOr,
    fail_block: BlockId,
) -> Result<(), LowerError> {
    let Some((last, init)) = pattern.patterns.split_last() else {
        return Err(LowerError::internal("or-pattern without alternatives"));
    };

    let mut expected_names: Option<BTreeSet<String>> = None;
    for alternative in &pattern.patterns {
        let mut names = BTreeSet::new();
        collect_bound_names(alternative, &mut names)?;
        match &expected_names {
            Some(expected) if *expected != names => {
                return Err(LowerError::unsupported_at(
                    "alternative patterns bind different names",
                    SourceSpan::from_bounds(pattern.range.start().to_u32(), pattern.range.end().to_u32()),
                ));
            }
            Some(_) => {}
            None => expected_names = Some(names),
        }
    }

    let matched_block = scope.alloc_block()?;
    for alternative in init {
        let alt_fail = scope.alloc_block()?;
        lower_pattern(driver, scope, subject, alternative, alt_fail)?;
        scope.jump_if_open(matched_block)?;
        scope.switch_to(alt_fail)?;
    }
    lower_pattern(driver, scope, subject, last, fail_block)?;
    scope.jump_if_open(matched_block)?;
    scope.switch_to(matched_block)
}

fn lower_sequence_pattern(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    subject: Value,
    patterns: &[Pattern],
    fail_block: BlockId,
) -> Result<(), LowerError> {
    let is_seq = scope.emit(InstKind::MatchSequence { subj: subject })?;
    branch_on(scope, is_seq, fail_block)?;

    let star_index = patterns
        .iter()
        .position(|nested| matches!(nested, Pattern::MatchStar(_)));
    if patterns
        .iter()
        .filter(|nested| matches!(nested, Pattern::MatchStar(_)))
        .count()
        > 1
    {
        return Err(LowerError::unsupported(
            "multiple starred subpatterns in sequence pattern",
        ));
    }

    match star_index {
        None => {
            let is_len = scope.emit(InstKind::MatchLenGe {
                subj: subject,
                n: patterns.len(),
                exact: true,
            })?;
            branch_on(scope, is_len, fail_block)?;
            for (index, nested) in patterns.iter().enumerate() {
                if is_wildcard(nested) {
                    continue;
                }
                let item = driver.lower_sequence_item(scope, subject, index as i64)?;
                lower_pattern(driver, scope, item, nested, fail_block)?;
            }
        }
        Some(star_index) => {
            let before = star_index;
            let after = patterns.len() - star_index - 1;
            let required = before + after;
            if required > 0 {
                let is_len = scope.emit(InstKind::MatchLenGe {
                    subj: subject,
                    n: required,
                    exact: false,
                })?;
                branch_on(scope, is_len, fail_block)?;
            }
            for (index, nested) in patterns[..before].iter().enumerate() {
                if is_wildcard(nested) {
                    continue;
                }
                let item = driver.lower_sequence_item(scope, subject, index as i64)?;
                lower_pattern(driver, scope, item, nested, fail_block)?;
            }
            if let Pattern::MatchStar(star) = &patterns[star_index] {
                if let Some(name) = star.name.as_ref() {
                    let rest = driver.lower_sequence_rest(scope, subject, before as i64, after as i64)?;
                    store_capture(driver, scope, name, rest)?;
                }
            }
            for (offset, nested) in patterns[star_index + 1..].iter().enumerate() {
                if is_wildcard(nested) {
                    continue;
                }
                let index = -((after - offset) as i64);
                let item = driver.lower_sequence_item(scope, subject, index)?;
                lower_pattern(driver, scope, item, nested, fail_block)?;
            }
        }
    }
    Ok(())
}

fn lower_mapping_pattern(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    subject: Value,
    pattern: &ruff_python_ast::PatternMatchMapping,
    fail_block: BlockId,
) -> Result<(), LowerError> {
    if pattern.keys.len() != pattern.patterns.len() {
        return Err(LowerError::internal("mapping pattern key/pattern arity mismatch"));
    }

    let is_map = scope.emit(InstKind::MatchMapping { subj: subject })?;
    branch_on(scope, is_map, fail_block)?;

    if !pattern.keys.is_empty() {
        let is_len = scope.emit(InstKind::MatchLenGe {
            subj: subject,
            n: pattern.keys.len(),
            exact: false,
        })?;
        branch_on(scope, is_len, fail_block)?;
    }

    let mut keys = Vec::with_capacity(pattern.keys.len());
    for key in &pattern.keys {
        keys.push(driver.lower_expr(scope, key)?);
    }

    if !keys.is_empty() {
        let extracted = scope.emit(InstKind::MatchKeys {
            subj: subject,
            keys: keys.clone(),
        })?;
        let none = scope.emit(InstKind::Const(PyConst::None))?;
        let found = scope.emit(InstKind::Is {
            lhs: extracted,
            rhs: none,
            negate: true,
        })?;
        branch_on(scope, found, fail_block)?;

        for (index, nested) in pattern.patterns.iter().enumerate() {
            if is_wildcard(nested) {
                continue;
            }
            let item = driver.lower_sequence_item(scope, extracted, index as i64)?;
            lower_pattern(driver, scope, item, nested, fail_block)?;
        }
    }

    if let Some(rest) = pattern.rest.as_ref() {
        let rest_map = scope.emit(InstKind::BuildMap { pairs: Vec::new() })?;
        scope.emit(InstKind::DictMerge {
            map: rest_map,
            other: subject,
        })?;
        for key in &keys {
            scope.emit(InstKind::SubscriptDel {
                obj: rest_map,
                index: *key,
            })?;
        }
        store_capture(driver, scope, rest, rest_map)?;
    }
    Ok(())
}

fn lower_class_pattern(
    driver: &mut LoweringDriver,
    scope: &mut BodyScope,
    subject: Value,
    pattern: &ruff_python_ast::PatternMatchClass,
    fail_block: BlockId,
) -> Result<(), LowerError> {
    let cls = driver.lower_expr(scope, &pattern.cls)?;
    let positional = &pattern.arguments.patterns;
    let keywords = &pattern.arguments.keywords;

    let mut kw_names = Vec::with_capacity(keywords.len());
    for keyword in keywords {
        kw_names.push(driver.names.intern(keyword.attr.as_str())?);
    }

    let extracted = scope.emit(InstKind::MatchClass {
        subj: subject,
        cls,
        nargs: positional.len(),
        kw: kw_names,
    })?;
    let none = scope.emit(InstKind::Const(PyConst::None))?;
    let matched = scope.emit(InstKind::Is {
        lhs: extracted,
        rhs: none,
        negate: true,
    })?;
    branch_on(scope, matched, fail_block)?;

    for (index, nested) in positional.iter().enumerate() {
        if is_wildcard(nested) {
            continue;
        }
        let item = driver.lower_sequence_item(scope, extracted, index as i64)?;
        lower_pattern(driver, scope, item, nested, fail_block)?;
    }
    for (offset, keyword) in keywords.iter().enumerate() {
        if is_wildcard(&keyword.pattern) {
            continue;
        }
        let index = (positional.len() + offset) as i64;
        let item = driver.lower_sequence_item(scope, extracted, index)?;
        lower_pattern(driver, scope, item, &keyword.pattern, fail_block)?;
    }
    Ok(())
}

/// Rejects patterns that bind the same name more than once, mirroring
/// CPython's compile-time "multiple assignments to name" check.
fn validate_case_pattern(pattern: &Pattern) -> Result<(), LowerError> {
    let mut names = BTreeSet::new();
    collect_bound_names(pattern, &mut names)
}

/// Collects capture names bound by `pattern` into `names`, failing on
/// duplicates.  Or-pattern alternatives bind the same set on every path
/// (enforced during lowering), so only the first alternative contributes.
fn collect_bound_names(pattern: &Pattern, names: &mut BTreeSet<String>) -> Result<(), LowerError> {
    fn bind(names: &mut BTreeSet<String>, identifier: &Identifier) -> Result<(), LowerError> {
        if !names.insert(identifier.as_str().to_owned()) {
            return Err(LowerError::unsupported(format!(
                "multiple assignments to name '{}' in pattern",
                identifier.as_str()
            )));
        }
        Ok(())
    }
    match pattern {
        Pattern::MatchValue(_) | Pattern::MatchSingleton(_) => Ok(()),
        Pattern::MatchAs(pattern) => {
            if let Some(nested) = pattern.pattern.as_deref() {
                collect_bound_names(nested, names)?;
            }
            if let Some(name) = pattern.name.as_ref() {
                bind(names, name)?;
            }
            Ok(())
        }
        Pattern::MatchStar(pattern) => {
            if let Some(name) = pattern.name.as_ref() {
                bind(names, name)?;
            }
            Ok(())
        }
        Pattern::MatchSequence(pattern) => {
            for nested in &pattern.patterns {
                collect_bound_names(nested, names)?;
            }
            Ok(())
        }
        Pattern::MatchMapping(pattern) => {
            for nested in &pattern.patterns {
                collect_bound_names(nested, names)?;
            }
            if let Some(rest) = pattern.rest.as_ref() {
                bind(names, rest)?;
            }
            Ok(())
        }
        Pattern::MatchClass(pattern) => {
            for nested in &pattern.arguments.patterns {
                collect_bound_names(nested, names)?;
            }
            for keyword in &pattern.arguments.keywords {
                collect_bound_names(&keyword.pattern, names)?;
            }
            Ok(())
        }
        Pattern::MatchOr(pattern) => {
            let Some(first) = pattern.patterns.first() else {
                return Err(LowerError::internal("or-pattern without alternatives"));
            };
            for alternative in &pattern.patterns[1..] {
                let mut scratch = BTreeSet::new();
                collect_bound_names(alternative, &mut scratch)?;
            }
            collect_bound_names(first, names)
        }
    }
}
