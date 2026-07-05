//! Native `_suggestions` typo helper.
//!
//! This ports the bounded Levenshtein search used by CPython's
//! `Python/suggestions.c` and mirrored in `Lib/traceback.py`, so traceback can
//! ask for the best candidate without importing `difflib` for the common path.

use super::{builtins_mod::VARIADIC_ARITY, install_module};
use crate::{
	intern::intern,
	object::PyObject,
	types::{dict, exc::ExceptionKind, list::PyList, type_},
};

const MAX_CANDIDATE_ITEMS: usize = 750;
const MAX_STRING_SIZE: usize = 40;
const MOVE_COST: usize = 2;
const CASE_COST: usize = 1;

type BuiltinFn = unsafe extern "C" fn(*mut *mut PyObject, usize) -> *mut PyObject;

pub(super) fn make_module() -> Result<*mut PyObject, String> {
	install_module("_suggestions", [
		(intern("__name__"), str_object("_suggestions")?),
		function_attr("_generate_suggestions", generate_suggestions_entry)?,
	])
}

unsafe extern "C" fn generate_suggestions_entry(
	argv: *mut *mut PyObject,
	argc: usize,
) -> *mut PyObject {
	if argc != 2 || argv.is_null() {
		return raise_type_error("_generate_suggestions() takes exactly 2 arguments");
	}
	let args = unsafe { std::slice::from_raw_parts(argv, argc) };
	let candidates = crate::tag::untag_arg(args[0]);
	if unsafe { dict::type_name(candidates) } != Some("list") {
		return raise_type_error("candidates must be a list");
	}
	let wrong_name = crate::tag::untag_arg(args[1]);
	let Some(wrong_name) = (unsafe { type_::unicode_text(wrong_name) }) else {
		return raise_type_error(&format!(
			"_generate_suggestions() argument 2 must be str, not {}",
			type_name(crate::tag::untag_arg(args[1]))
		));
	};

	let list = unsafe { &*candidates.cast::<PyList>() };
	let items = unsafe { list.as_slice() };
	if items.len() > MAX_CANDIDATE_ITEMS || wrong_name.chars().count() > MAX_STRING_SIZE {
		return unsafe { crate::abi::pon_none() };
	}

	let mut best_distance = wrong_name.chars().count();
	let mut suggestion: Option<&str> = None;
	for &item in items {
		let Some(candidate) = (unsafe { type_::unicode_text(crate::tag::untag_arg(item)) }) else {
			return raise_type_error("all elements in 'candidates' must be strings");
		};
		if candidate == wrong_name {
			continue;
		}
		let max_distance = ((candidate.chars().count() + wrong_name.chars().count() + 3) * MOVE_COST
			/ 6)
			.min(best_distance.saturating_sub(1));
		let current = levenshtein_distance(wrong_name, candidate, max_distance);
		if current > max_distance {
			continue;
		}
		if suggestion.is_none() || current < best_distance {
			suggestion = Some(candidate);
			best_distance = current;
		}
	}

	match suggestion {
		Some(text) => match str_object(text) {
			Ok(object) => object,
			Err(message) => crate::abi::return_null_with_error(message),
		},
		None => unsafe { crate::abi::pon_none() },
	}
}

fn levenshtein_distance(a: &str, b: &str, max_cost: usize) -> usize {
	if a == b {
		return 0;
	}

	let mut a: Vec<char> = a.chars().collect();
	let mut b: Vec<char> = b.chars().collect();

	let mut pre = 0;
	while pre < a.len() && pre < b.len() && a[pre] == b[pre] {
		pre += 1;
	}
	if pre != 0 {
		a.drain(0..pre);
		b.drain(0..pre);
	}

	let mut post = 0;
	while post < a.len() && post < b.len() && a[a.len() - 1 - post] == b[b.len() - 1 - post] {
		post += 1;
	}
	if post != 0 {
		a.truncate(a.len() - post);
		b.truncate(b.len() - post);
	}

	if a.is_empty() || b.is_empty() {
		return MOVE_COST * (a.len() + b.len());
	}
	if a.len() > MAX_STRING_SIZE || b.len() > MAX_STRING_SIZE {
		return max_cost + 1;
	}
	if b.len() < a.len() {
		std::mem::swap(&mut a, &mut b);
	}
	if (b.len() - a.len()) * MOVE_COST > max_cost {
		return max_cost + 1;
	}

	let mut row: Vec<usize> = (1..=a.len()).map(|index| index * MOVE_COST).collect();
	let mut result = 0;
	for (bindex, &bchar) in b.iter().enumerate() {
		let mut distance = bindex * MOVE_COST;
		result = distance;
		let mut minimum = usize::MAX;
		for (index, &achar) in a.iter().enumerate() {
			let substitute = distance + substitution_cost(bchar, achar);
			distance = row[index];
			let insert_delete = result.min(distance) + MOVE_COST;
			result = insert_delete.min(substitute);
			row[index] = result;
			minimum = minimum.min(result);
		}
		if minimum > max_cost {
			return max_cost + 1;
		}
	}
	result
}

fn substitution_cost(a: char, b: char) -> usize {
	if a == b {
		0
	} else if lower_eq(a, b) {
		CASE_COST
	} else {
		MOVE_COST
	}
}

fn lower_eq(a: char, b: char) -> bool {
	let mut lower_a = a.to_lowercase();
	let mut lower_b = b.to_lowercase();
	lower_a.next() == lower_b.next() && lower_a.next().is_none() && lower_b.next().is_none()
}

fn function_attr(name: &str, entry: BuiltinFn) -> Result<(u32, *mut PyObject), String> {
	let function =
		unsafe { crate::abi::pon_make_function(entry as *const u8, VARIADIC_ARITY, intern(name)) };
	(!function.is_null())
		.then_some((intern(name), function))
		.ok_or_else(|| format!("failed to allocate _suggestions.{name}"))
}

fn str_object(text: &str) -> Result<*mut PyObject, String> {
	let object = unsafe { crate::abi::pon_const_str(text.as_ptr(), text.len()) };
	(!object.is_null())
		.then_some(object)
		.ok_or_else(|| format!("failed to allocate string {text:?}"))
}

fn type_name(object: *mut PyObject) -> &'static str {
	unsafe { dict::type_name(object) }.unwrap_or("object")
}

fn raise_type_error(message: &str) -> *mut PyObject {
	crate::abi::exc::raise_kind_error_text(ExceptionKind::TypeError, message)
}
