use std::ptr;

use super::load_extension_module;
use super::tests::{ResetImportStateOnDrop, TempExtensionRoot, compile_extension};
use crate::abi::str_::pon_const_bytes;
use crate::abi::{format_object_for_print, pon_call, pon_const_int, pon_const_str, pon_runtime_init};
use crate::import::{module_attr, reset_import_state_for_tests};
use crate::intern::intern;
use crate::thread_state::{pon_err_clear, pon_err_message, test_state_lock};

#[test]
fn args_parse_and_buildvalue_extension_paths() {
    let _guard = test_state_lock();
    let _reset = ResetImportStateOnDrop;
    unsafe {
        assert_eq!(pon_runtime_init(), 0);
    }
    pon_err_clear();
    reset_import_state_for_tests();

    let temp = TempExtensionRoot::new();
    let module_path = compile_extension(
        &temp,
        "capi_args_ext",
        r#"
#include <Python.h>

static int double_converter(PyObject *object, void *out) {
    long value = PyLong_AsLong(object);
    if (value == -1 && PyErr_Occurred()) {
        return 0;
    }
    *(long *)out = value * 2;
    return 1;
}

static PyObject *parse_optional(PyObject *self, PyObject *args) {
    (void)self;
    int left = 0;
    int right = 0;
    const char *label = "fallback";
    if (!PyArg_ParseTuple(args, "ii|s:parse_optional", &left, &right, &label)) {
        return NULL;
    }
    long score = left + right + (label[0] == 'x' ? 10 : 0);
    return PyLong_FromLong(score);
}

static PyObject *parse_type_checked(PyObject *self, PyObject *args) {
    (void)self;
    PyObject *value = NULL;
    if (!PyArg_ParseTuple(args, "O!:parse_type_checked", &PyUnicode_Type, &value)) {
        return NULL;
    }
    Py_INCREF(value);
    return value;
}

static PyObject *parse_converted(PyObject *self, PyObject *args) {
    (void)self;
    long doubled = 0;
    if (!PyArg_ParseTuple(args, "O&:parse_converted", double_converter, &doubled)) {
        return NULL;
    }
    return PyLong_FromLong(doubled);
}

static PyObject *parse_s_hash(PyObject *self, PyObject *args) {
    (void)self;
    char *buffer = NULL;
    Py_ssize_t length = 0;
    if (!PyArg_ParseTuple(args, "s#:parse_s_hash", &buffer, &length)) {
        return NULL;
    }
    return PyLong_FromLong((long)length + (unsigned char)buffer[0]);
}

static PyObject *parse_truth(PyObject *self, PyObject *args) {
    (void)self;
    int truth = -1;
    if (!PyArg_ParseTuple(args, "p:parse_truth", &truth)) {
        return NULL;
    }
    return PyLong_FromLong(truth);
}

static PyObject *parse_keywords(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    PyObject *inner_args = Py_BuildValue("(i)", 3);
    PyObject *kwargs = PyDict_New();
    PyObject *scale_object = PyLong_FromLong(4);
    if (inner_args == NULL || kwargs == NULL || scale_object == NULL) {
        return NULL;
    }
    if (PyDict_SetItemString(kwargs, "scale", scale_object) < 0) {
        return NULL;
    }
    static char *kwlist[] = {"value", "scale", NULL};
    int value = 0;
    int scale = 1;
    if (!PyArg_ParseTupleAndKeywords(inner_args, kwargs, "i|$i:parse_keywords", kwlist, &value, &scale)) {
        return NULL;
    }
    return PyLong_FromLong(value * scale);
}

static PyObject *unpack_pair(PyObject *self, PyObject *args) {
    (void)self;
    PyObject *left = NULL;
    PyObject *right = NULL;
    if (!PyArg_UnpackTuple(args, "unpack_pair", 2, 2, &left, &right)) {
        return NULL;
    }
    long left_value = PyLong_AsLong(left);
    if (left_value == -1 && PyErr_Occurred()) {
        return NULL;
    }
    long right_value = PyLong_AsLong(right);
    if (right_value == -1 && PyErr_Occurred()) {
        return NULL;
    }
    return PyLong_FromLong(left_value * 10 + right_value);
}

static PyObject *build_nested(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    return Py_BuildValue("(i[sd]{s:i})", 7, "name", 2.5, "answer", 4);
}

static PyObject *arity_two(PyObject *self, PyObject *args) {
    (void)self;
    int left = 0;
    int right = 0;
    if (!PyArg_ParseTuple(args, "ii:arity_two", &left, &right)) {
        return NULL;
    }
    return PyLong_FromLong(left + right);
}

static PyMethodDef methods[] = {
    {"parse_optional", parse_optional, METH_VARARGS, "parse ii|s"},
    {"parse_type_checked", parse_type_checked, METH_VARARGS, "parse O!"},
    {"parse_converted", parse_converted, METH_VARARGS, "parse O&"},
    {"parse_s_hash", parse_s_hash, METH_VARARGS, "parse s#"},
    {"parse_truth", parse_truth, METH_VARARGS, "parse p"},
    {"parse_keywords", parse_keywords, METH_VARARGS, "parse keywords"},
    {"unpack_pair", unpack_pair, METH_VARARGS, "unpack tuple"},
    {"build_nested", build_nested, METH_VARARGS, "build nested value"},
    {"arity_two", arity_two, METH_VARARGS, "arity parity"},
    {NULL, NULL, 0, NULL}
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "capi_args_ext",
    "Pon C-API args test extension",
    -1,
    methods
};

PyMODINIT_FUNC PyInit_capi_args_ext(void) {
    return PyModule_Create(&module);
}
"#,
    );

    let module = load_extension_module("capi_args_ext", &module_path)
        .unwrap_or_else(|message| panic!("failed to load args C extension: {message}"));
    assert!(!module.is_null(), "extension loader returned NULL module");
    let module_name = intern("capi_args_ext");

    let call_noargs = |name: &str| {
        let function = module_attr(module_name, intern(name)).unwrap_or_else(|| panic!("{name} registered"));
        let result = unsafe { pon_call(function, ptr::null_mut(), 0) };
        assert!(!result.is_null(), "{name} returned NULL: {:?}", pon_err_message());
        result
    };
    let call_with_args = |name: &str, argv: &mut [*mut crate::object::PyObject]| {
        let function = module_attr(module_name, intern(name)).unwrap_or_else(|| panic!("{name} registered"));
        let result = unsafe { pon_call(function, argv.as_mut_ptr(), argv.len()) };
        assert!(!result.is_null(), "{name} returned NULL: {:?}", pon_err_message());
        result
    };

    let mut optional_args = [unsafe { pon_const_int(1) }, unsafe { pon_const_int(2) }];
    let optional = call_with_args("parse_optional", &mut optional_args);
    assert_eq!(format_object_for_print(optional).as_deref(), Ok("3"));

    let label = unsafe { pon_const_str(b"x".as_ptr(), 1) };
    let mut optional_with_label = [unsafe { pon_const_int(1) }, unsafe { pon_const_int(2) }, label];
    let optional = call_with_args("parse_optional", &mut optional_with_label);
    assert_eq!(format_object_for_print(optional).as_deref(), Ok("13"));

    let text = unsafe { pon_const_str(b"ok".as_ptr(), 2) };
    let mut type_args = [text];
    let checked = call_with_args("parse_type_checked", &mut type_args);
    assert_eq!(format_object_for_print(checked).as_deref(), Ok("ok"));

    let mut converter_args = [unsafe { pon_const_int(6) }];
    let converted = call_with_args("parse_converted", &mut converter_args);
    assert_eq!(format_object_for_print(converted).as_deref(), Ok("12"));

    let bytes = unsafe { pon_const_bytes(b"abc".as_ptr(), 3) };
    let mut bytes_args = [bytes];
    let sized = call_with_args("parse_s_hash", &mut bytes_args);
    assert_eq!(format_object_for_print(sized).as_deref(), Ok("100"));

    let mut truth_args = [unsafe { pon_const_int(0) }];
    let truth = call_with_args("parse_truth", &mut truth_args);
    assert_eq!(format_object_for_print(truth).as_deref(), Ok("0"));

    let keywords = call_noargs("parse_keywords");
    assert_eq!(format_object_for_print(keywords).as_deref(), Ok("12"));

    let mut unpack_args = [unsafe { pon_const_int(4) }, unsafe { pon_const_int(2) }];
    let unpacked = call_with_args("unpack_pair", &mut unpack_args);
    assert_eq!(format_object_for_print(unpacked).as_deref(), Ok("42"));

    let nested = call_noargs("build_nested");
    assert_eq!(format_object_for_print(nested).as_deref(), Ok("(7, ['name', 2.5], {'answer': 4})"));

    let arity = module_attr(module_name, intern("arity_two")).expect("arity_two registered");
    let mut too_few = [unsafe { pon_const_int(1) }];
    pon_err_clear();
    let result = unsafe { pon_call(arity, too_few.as_mut_ptr(), too_few.len()) };
    assert!(result.is_null(), "arity_two unexpectedly succeeded: {:?}", format_object_for_print(result));
    assert_eq!(
        pon_err_message().as_deref(),
        Some("TypeError: arity_two() takes exactly 2 arguments (1 given)"),
        "PyArg_ParseTuple arity TypeError must match python3.14 getargs.c"
    );
}
