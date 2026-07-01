use std::ptr;
use std::sync::{LazyLock, Mutex};

use pon_runtime::abi::iter::pon_get_iter;
use pon_runtime::abi::number::{NumberOp, pon_binary_op};
use pon_runtime::abi::object::{RichCompareOp, pon_is_true, pon_rich_compare};
use pon_runtime::abi::seq::{pon_build_list, pon_get_len};
use pon_runtime::abi::{
    HELPERS, format_object_for_print, pon_binary_add, pon_const_int, pon_const_str,
    pon_runtime_init,
};
use pon_runtime::{pon_err_clear, pon_err_message, pon_err_occurred};

static RUNTIME_TEST_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

const NUMBER_ADD: NumberOp = 0;

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

fn assert_prints(object: *mut pon_runtime::PyObject, expected: &str) {
    assert_eq!(format_object_for_print(object).as_deref(), Ok(expected));
}

fn assert_pending_error_mentions_any(expected_fragments: &[&str]) {
    assert!(pon_err_occurred(), "expected an exception to be pending");

    let message = pon_err_message().unwrap_or_default().to_ascii_lowercase();
    assert!(
        expected_fragments.iter().any(|fragment| message.contains(fragment)),
        "expected diagnostic {message:?} to mention one of {expected_fragments:?}",
    );
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
        assert!(
            legacy.is_null(),
            "pon_binary_add(int, str) should fail with the NULL sentinel",
        );
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
        assert_eq!(
            helper_count, 1,
            "helper table must expose exactly one pon_get_len symbol"
        );
        assert_eq!(helper_address, pon_get_len as *const ());

        let length = pon_get_len(list, ptr::null_mut());
        assert!(!length.is_null(), "pon_get_len(list) returned NULL");
        assert!(!pon_err_occurred(), "pon_get_len(list) left an exception pending");
        assert_prints(length, "3");
        assert_eq!(pon_is_true(length), 1, "non-zero length should be truthy");
        assert!(!pon_err_occurred(), "pon_is_true(length) left an exception pending");
    });
}
