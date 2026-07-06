//! Human-readable IR dumps for debugging tools.
//!
//! Backs the internal `_pon_debug.ir(function)` view. The output is a
//! debugging aid, not a stable serialization format: instruction payloads use
//! their `Debug` form with id operands rewritten for readability (`Value(3)`
//! becomes `v3`, `BlockId(2)` becomes `block2`, `NameId(0)` becomes `@print`,
//! and so on). String constants that happen to contain such spellings are
//! rewritten too; tolerated for a debug dump.

use std::fmt::Write;

use crate::ir::{Function, Module, Terminator};

/// Render the function at `index` in `module.functions` as indented text.
///
/// Returns `None` when `index` is out of range. Called by the JIT's
/// `_pon_debug` inspection backend.
#[must_use]
pub fn function_text(module: &Module, index: usize) -> Option<String> {
	let function = module.functions.get(index)?;
	let mut out = String::new();
	let _ = write!(out, "fn{index} {}({})", function.name, signature(function));
	let _ = write!(out, " n_locals={}", function.n_locals);
	if function.is_async_generator {
		out.push_str(" [async generator]");
	} else if function.is_coroutine {
		out.push_str(" [coroutine]");
	} else if function.is_generator {
		out.push_str(" [generator]");
	}
	out.push('\n');

	let mut last_line = 0;
	for block in &function.blocks {
		let _ = writeln!(out, "block{}:", block.id.0);
		for inst in &block.insts {
			let _ = write!(
				out,
				"  v{} = {}",
				inst.result.0,
				rewrite_ids(&format!("{:?}", inst.kind), module)
			);
			let mut notes = Vec::new();
			if inst.line != 0 && inst.line != last_line {
				notes.push(format!("line {}", inst.line));
				last_line = inst.line;
			}
			if let Some(slot) = inst.feedback_slot {
				notes.push(format!("fb{}", slot.0));
			}
			if inst.inferred_type != crate::types::Type::Bottom
				|| inst.static_type != crate::types::Type::Object
			{
				notes.push(format!("ty {:?}/{:?}", inst.inferred_type, inst.static_type));
			}
			if !notes.is_empty() {
				let _ = write!(out, "  ; {}", notes.join(", "));
			}
			out.push('\n');
		}
		let _ = writeln!(out, "  {}", terminator_text(&block.term));
	}
	Some(out)
}

/// Reconstruct a Python-style parameter list from the lowered layout.
fn signature(function: &Function) -> String {
	let params = &function.params;
	let positional = params.positional_only_count + params.positional_count;
	let mut parts: Vec<String> = Vec::new();
	for (index, name) in params.names.iter().enumerate() {
		if index == positional {
			// Keyword-only parameters follow; emit the separating star (or the
			// real `*args` slot) exactly once.
			match &params.vararg_name {
				Some(vararg) => parts.push(format!("*{vararg}")),
				None => parts.push("*".to_owned()),
			}
		}
		parts.push(name.clone());
		if params.positional_only_count > 0 && index + 1 == params.positional_only_count {
			parts.push("/".to_owned());
		}
	}
	if params.names.len() <= positional
		&& let Some(vararg) = &params.vararg_name
	{
		parts.push(format!("*{vararg}"));
	}
	if let Some(kwarg) = &params.kwarg_name {
		parts.push(format!("**{kwarg}"));
	}
	parts.join(", ")
}

fn terminator_text(term: &Terminator) -> String {
	match term {
		Terminator::Return(value) => format!("return v{}", value.0),
		Terminator::Jump(block) => format!("jump block{}", block.0),
		Terminator::Branch { cond, then_blk, else_blk } => {
			format!("branch v{} ? block{} : block{}", cond.0, then_blk.0, else_blk.0)
		},
		Terminator::CondBranch { cond, then_, else_ } => {
			format!("branch v{} ? block{} : block{}", cond.0, then_.0, else_.0)
		},
		Terminator::ForLoop { iter, body, done } => {
			format!("for v{} ? block{} : block{}", iter.0, body.0, done.0)
		},
		Terminator::Suspend { state, val, resume } => {
			format!("suspend state={state} val=v{} resume=block{}", val.0, resume.0)
		},
		Terminator::RaiseTerm => "raise".to_owned(),
		Terminator::Unreachable => "unreachable".to_owned(),
	}
}

/// Id-newtype `Debug` spellings and their rewritten prefixes.
const ID_MARKERS: &[(&str, &str)] = &[
	("Value(", "v"),
	("BlockId(", "block"),
	("LocalId(", "local"),
	("CellId(", "cell"),
	("FeedbackSlot(", "fb"),
];

/// Rewrite id-newtype `Debug` spellings into compact readable operands.
fn rewrite_ids(text: &str, module: &Module) -> String {
	let mut out = String::with_capacity(text.len());
	let mut rest = text;
	let mut prev_is_word = false;
	'scan: while !rest.is_empty() {
		if !prev_is_word && let Some((consumed, replacement)) = match_id(rest, module) {
			out.push_str(&replacement);
			rest = &rest[consumed..];
			prev_is_word = replacement
				.chars()
				.last()
				.is_some_and(|ch| ch.is_alphanumeric() || ch == '_');
			continue 'scan;
		}
		let ch = rest.chars().next().expect("non-empty rest");
		out.push(ch);
		prev_is_word = ch.is_alphanumeric() || ch == '_';
		rest = &rest[ch.len_utf8()..];
	}
	out
}

/// Match one `Ident(N)` id spelling at the start of `rest`; returns the
/// consumed byte count and the replacement text.
fn match_id(rest: &str, module: &Module) -> Option<(usize, String)> {
	for (marker, prefix) in ID_MARKERS {
		if let Some((consumed, id)) = match_marker(rest, marker) {
			return Some((consumed, format!("{prefix}{id}")));
		}
	}
	if let Some((consumed, id)) = match_marker(rest, "NameId(") {
		return Some((consumed, name_text(module, id)));
	}
	if let Some((consumed, id)) = match_marker(rest, "FunctionId(") {
		let replacement = module
			.functions
			.get(id as usize)
			.map_or_else(|| format!("fn{id}"), |function| format!("fn{id}<{}>", function.name));
		return Some((consumed, replacement));
	}
	None
}

/// Match `marker` followed by digits and `)`; returns consumed bytes and the
/// id.
fn match_marker(rest: &str, marker: &str) -> Option<(usize, u32)> {
	let tail = rest.strip_prefix(marker)?;
	let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
	if digits == 0 || tail.as_bytes().get(digits) != Some(&b')') {
		return None;
	}
	let id: u32 = tail[..digits].parse().ok()?;
	Some((marker.len() + digits + 1, id))
}

/// Resolve a name-table id to `@name`, quoting non-identifier spellings.
fn name_text(module: &Module, id: u32) -> String {
	let Some(name) = module.names.get(id as usize) else {
		return format!("@?{id}");
	};
	let plain = !name.is_empty()
		&& name
			.chars()
			.all(|ch| ch.is_alphanumeric() || ch == '_' || ch == '.');
	if plain {
		format!("@{name}")
	} else {
		format!("@{name:?}")
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ir::{
		Block, BlockId, Function, FunctionId, Inst, InstKind, LocalId, Module, NameId, ParamLayout,
		PyConst, Terminator, Value,
	};

	fn one_function_module(function: Function) -> Module {
		Module {
			functions: vec![function],
			main:      FunctionId(0),
			names:     vec!["print".to_owned()],
		}
	}

	#[test]
	fn renders_blocks_values_and_names() {
		let function = Function {
			name:               "demo".to_owned(),
			arity:              1,
			is_coroutine:       false,
			is_generator:       false,
			is_async_generator: false,
			params:             ParamLayout {
				names: vec!["x".to_owned()],
				positional_count: 1,
				..ParamLayout::default()
			},
			blocks:             vec![Block {
				id:    BlockId(0),
				insts: vec![
					Inst::new(Value(0), InstKind::LoadLocal(LocalId(0))),
					Inst::new(Value(1), InstKind::LoadGlobal(NameId(0))),
				],
				term:  Terminator::Return(Value(1)),
			}],
			n_locals:           1,
		};
		let text = function_text(&one_function_module(function), 0).expect("function exists");
		assert!(text.starts_with("fn0 demo(x) n_locals=1\n"), "header: {text}");
		assert!(text.contains("block0:\n"), "block label: {text}");
		assert!(text.contains("v0 = LoadLocal(local0)"), "local rewrite: {text}");
		assert!(text.contains("v1 = LoadGlobal(@print)"), "name rewrite: {text}");
		assert!(text.contains("return v1"), "terminator: {text}");
	}

	#[test]
	fn out_of_range_index_is_none() {
		let module =
			Module { functions: Vec::new(), main: FunctionId(0), names: Vec::new() };
		assert!(function_text(&module, 0).is_none());
	}

	#[test]
	fn string_constants_render_verbatim() {
		let function = Function {
			name:               "s".to_owned(),
			arity:              0,
			is_coroutine:       false,
			is_generator:       false,
			is_async_generator: false,
			params:             ParamLayout::default(),
			blocks:             vec![Block {
				id:    BlockId(0),
				insts: vec![Inst::new(
					Value(0),
					InstKind::Const(PyConst::Str("hello world".to_owned())),
				)],
				term:  Terminator::Return(Value(0)),
			}],
			n_locals:           0,
		};
		let text = function_text(&one_function_module(function), 0).expect("function exists");
		assert!(text.contains("Const(Str(\"hello world\"))"), "const: {text}");
	}
}
