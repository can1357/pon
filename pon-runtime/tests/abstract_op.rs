use std::{
	ptr,
	sync::{LazyLock, Mutex},
};

use pon_runtime::{
	PyObject,
	abi::{
		HELPERS, format_object_for_print,
		iter::pon_get_iter,
		number::{NumberOp, pon_binary_op},
		object::{RichCompareOp, pon_is_true, pon_rich_compare},
		pon_binary_add, pon_const_int, pon_const_str, pon_runtime_init,
		seq::{pon_build_list, pon_get_len},
	},
	pon_err_clear, pon_err_message, pon_err_occurred,
	tag::{self, SMALL_INT_MAX, SMALL_INT_MIN},
	types::int,
};

static RUNTIME_TEST_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

const NUMBER_ADD: NumberOp = 0;
const NUMBER_SUB: NumberOp = 1;
const NUMBER_MUL: NumberOp = 2;
const NUMBER_AND: NumberOp = 10;
const NUMBER_OR: NumberOp = 11;
const NUMBER_XOR: NumberOp = 12;

const RICH_LT: RichCompareOp = 0;
const RICH_EQ: RichCompareOp = 2;
const RICH_GT: RichCompareOp = 4;

fn with_clean_runtime(test: impl FnOnce()) {
	let _guard = RUNTIME_TEST_LOCK
		.lock()
		.unwrap_or_else(|poison| poison.into_inner());

	unsafe {
		assert_eq!(pon_runtime_init(), 0);
	}
	pon_err_clear();

	test();

	pon_err_clear();
}

unsafe fn const_int(value: i64) -> *mut pon_runtime::PyObject {
	let object = unsafe { pon_const_int(value) };
	assert!(!object.is_null(), "pon_const_int({value}) returned NULL");
	assert!(!pon_err_occurred(), "pon_const_int({value}) left an exception pending");
	object
}

unsafe fn const_str(bytes: &[u8]) -> *mut pon_runtime::PyObject {
	let object = unsafe { pon_const_str(bytes.as_ptr(), bytes.len()) };
	assert!(!object.is_null(), "pon_const_str returned NULL");
	assert!(!pon_err_occurred(), "pon_const_str left an exception pending");
	object
}

unsafe extern "C" fn prelude_passthrough(object: *mut PyObject) -> *mut PyObject {
	pon_runtime::untag_prelude!(object);
	object
}

#[track_caller]
fn assert_tagged_int_value(object: *mut PyObject, expected: i64) {
	assert!(tag::is_small_int(object), "expected tagged immediate for {expected}");
	assert_eq!(tag::untag_small_int(object), expected);
}

#[track_caller]
fn assert_heap_int_value(object: *mut PyObject, expected: &str) {
	assert!(!object.is_null(), "expected heap int {expected}, got NULL");
	assert!(tag::is_heap(object), "expected heap pointer for {expected}");
	assert!(!tag::is_small_int(object), "heap int {expected} must not be tagged");
	assert!(unsafe { int::is_exact_int(object) }, "expected exact PyLong for {expected}");
	assert_eq!(
		unsafe { int::to_bigint(object) }.map(|value| value.to_string()).as_deref(),
		Some(expected),
	);
}

fn assert_prints(object: *mut pon_runtime::PyObject, expected: &str) {
	assert_eq!(format_object_for_print(object).as_deref(), Ok(expected));
}

fn assert_pending_error_mentions_any(expected_fragments: &[&str]) {
	assert!(pon_err_occurred(), "expected an exception to be pending");

	let message = pon_err_message().unwrap_or_default().to_ascii_lowercase();
	assert!(
		expected_fragments
			.iter()
			.any(|fragment| message.contains(fragment)),
		"expected diagnostic {message:?} to mention one of {expected_fragments:?}",
	);
}

#[test]
fn const_int_tags_in_range_and_boxes_adjacent_out_of_range_i64s() {
	with_clean_runtime(|| unsafe {
		for value in [
			SMALL_INT_MIN,
			SMALL_INT_MIN + 1,
			-1,
			0,
			1,
			SMALL_INT_MAX - 1,
			SMALL_INT_MAX,
		] {
			let object = const_int(value);
			assert_tagged_int_value(object, value);
		}

		for value in [SMALL_INT_MIN - 1, SMALL_INT_MAX + 1] {
			let object = const_int(value);
			assert_heap_int_value(object, &value.to_string());
		}
	});
}

#[test]
fn untag_arg_and_prelude_box_tagged_ints_to_heap_pylongs() {
	with_clean_runtime(|| unsafe {
		let tagged = const_int(-17);
		assert_tagged_int_value(tagged, -17);

		let direct = tag::untag_arg(tagged);
		assert_heap_int_value(direct, "-17");

		let through_prelude = prelude_passthrough(tagged);
		assert_heap_int_value(through_prelude, "-17");
	});
}

#[test]
fn numeric_helpers_keep_small_tagged_results_and_promote_large_int_results() {
	with_clean_runtime(|| unsafe {
		for (name, op, left, right, expected) in [
			("add", NUMBER_ADD, 20, 22, 42),
			("sub", NUMBER_SUB, 5, 9, -4),
			("mul", NUMBER_MUL, 7, -6, -42),
			("and", NUMBER_AND, 0b1100, 0b1010, 0b1000),
			("or", NUMBER_OR, 0b1100, 0b1010, 0b1110),
			("xor", NUMBER_XOR, 0b1100, 0b1010, 0b0110),
		] {
			pon_err_clear();
			let result = pon_binary_op(op, const_int(left), const_int(right), ptr::null_mut());
			assert!(!result.is_null(), "pon_binary_op({name}) returned NULL");
			assert!(!pon_err_occurred(), "pon_binary_op({name}) left an exception pending");
			assert_tagged_int_value(result, expected);
		}

		let just_outside_small = pon_binary_op(
			NUMBER_ADD,
			const_int(SMALL_INT_MAX),
			const_int(1),
			ptr::null_mut(),
		);
		let expected = (SMALL_INT_MAX as i128 + 1).to_string();
		assert_heap_int_value(just_outside_small, &expected);

		let factor = 3_037_000_500_i64;
		let large_product = pon_binary_op(NUMBER_MUL, const_int(factor), const_int(factor), ptr::null_mut());
		let expected = ((factor as i128) * (factor as i128)).to_string();
		assert_heap_int_value(large_product, &expected);
	});
}

#[test]
fn truth_and_rich_compare_accept_raw_tagged_ints() {
	with_clean_runtime(|| unsafe {
		let zero = const_int(0);
		let minus_three = const_int(-3);
		let two = const_int(2);
		assert_tagged_int_value(zero, 0);
		assert_tagged_int_value(minus_three, -3);
		assert_tagged_int_value(two, 2);

		assert_eq!(pon_is_true(zero), 0, "tagged zero should be false");
		assert!(!pon_err_occurred(), "pon_is_true(tagged zero) left an exception pending");
		assert_eq!(pon_is_true(minus_three), 1, "tagged non-zero int should be true");
		assert!(
			!pon_err_occurred(),
			"pon_is_true(tagged non-zero int) left an exception pending",
		);

		for (name, op, left, right, expected_truth) in [
			("-3 < 2", RICH_LT, minus_three, two, 1),
			("-3 == 0", RICH_EQ, minus_three, zero, 0),
			("2 > 0", RICH_GT, two, zero, 1),
		] {
			pon_err_clear();
			let comparison = pon_rich_compare(op, left, right, ptr::null_mut());
			assert!(!comparison.is_null(), "pon_rich_compare({name}) returned NULL");
			assert!(!pon_err_occurred(), "pon_rich_compare({name}) left an exception pending");
			assert_eq!(pon_is_true(comparison), expected_truth, "unexpected truth for {name}");
		}
	});
}

#[test]
fn binary_op_add_matches_legacy_binary_add_for_boxed_ints() {
	with_clean_runtime(|| unsafe {
		let one = const_int(1);
		let two = const_int(2);

		let dispatched = pon_binary_op(NUMBER_ADD, one, two, ptr::null_mut());
		assert!(!dispatched.is_null(), "pon_binary_op(Add, 1, 2) returned NULL");
		assert!(!pon_err_occurred(), "pon_binary_op(Add, 1, 2) left an exception pending");
		assert_prints(dispatched, "3");

		let legacy = pon_binary_add(one, two);
		assert!(!legacy.is_null(), "pon_binary_add(1, 2) returned NULL");
		assert!(!pon_err_occurred(), "pon_binary_add(1, 2) left an exception pending");
		assert_prints(legacy, "3");
	});
}

#[test]
fn rich_compare_on_boxed_ints_produces_truth_values_for_ordering_and_equality() {
	with_clean_runtime(|| unsafe {
		let one = const_int(1);
		let another_one = const_int(1);
		let two = const_int(2);

		for (name, op, left, right, expected_truth) in [
			("1 < 2", RICH_LT, one, two, 1),
			("2 < 1", RICH_LT, two, one, 0),
			("1 == 1", RICH_EQ, one, another_one, 1),
			("1 == 2", RICH_EQ, one, two, 0),
			("2 > 1", RICH_GT, two, one, 1),
			("1 > 2", RICH_GT, one, two, 0),
		] {
			pon_err_clear();

			let comparison = pon_rich_compare(op, left, right, ptr::null_mut());
			assert!(!comparison.is_null(), "pon_rich_compare({name}) returned NULL");
			assert!(!pon_err_occurred(), "pon_rich_compare({name}) left an exception pending");

			let truth = pon_is_true(comparison);
			assert_eq!(truth, expected_truth, "unexpected truth value for {name}");
			assert!(!pon_err_occurred(), "pon_is_true result for {name} left an exception pending");
		}
	});
}

#[test]
fn unsupported_binary_op_returns_null_and_sets_type_error() {
	with_clean_runtime(|| unsafe {
		let integer = const_int(1);
		let text = const_str(b"not-a-number");

		pon_err_clear();
		let dispatched = pon_binary_op(NUMBER_ADD, integer, text, ptr::null_mut());
		assert!(
			dispatched.is_null(),
			"pon_binary_op(Add, int, str, NULL) should fail with the NULL sentinel",
		);
		assert_pending_error_mentions_any(&["type", "unsupported"]);

		pon_err_clear();
		let legacy = pon_binary_add(integer, text);
		assert!(legacy.is_null(), "pon_binary_add(int, str) should fail with the NULL sentinel",);
		assert_pending_error_mentions_any(&["type", "unsupported"]);
	});
}

#[test]
fn get_iter_on_non_iterable_returns_null_and_sets_type_error() {
	with_clean_runtime(|| unsafe {
		let integer = const_int(1);

		pon_err_clear();
		let result = pon_get_iter(integer, ptr::null_mut());

		assert!(result.is_null(), "iter(int) should fail with the NULL sentinel");
		assert_pending_error_mentions_any(&["iter", "type"]);
	});
}

#[test]
fn get_len_helper_table_entry_returns_boxed_truthy_length_for_sized_object() {
	with_clean_runtime(|| unsafe {
		let mut values = [const_int(1), const_int(2), const_int(3)];
		let list = pon_build_list(values.as_mut_ptr(), values.len());
		assert!(!list.is_null(), "pon_build_list returned NULL");
		assert!(!pon_err_occurred(), "pon_build_list left an exception pending");

		let mut helper_count = 0;
		let mut helper_address = ptr::null();
		for helper in HELPERS
			.iter()
			.filter(|helper| helper.symbol == "pon_get_len")
		{
			helper_count += 1;
			helper_address = helper.address;
		}
		assert_eq!(helper_count, 1, "helper table must expose exactly one pon_get_len symbol");
		assert_eq!(helper_address, pon_get_len as *const ());

		let length = pon_get_len(list, ptr::null_mut());
		assert!(!length.is_null(), "pon_get_len(list) returned NULL");
		assert!(!pon_err_occurred(), "pon_get_len(list) left an exception pending");
		assert_prints(length, "3");
		assert_eq!(pon_is_true(length), 1, "non-zero length should be truthy");
		assert!(!pon_err_occurred(), "pon_is_true(length) left an exception pending");
	});
}
