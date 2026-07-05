use std::ptr;

use super::{
	load_extension_module,
	tests::{ResetImportStateOnDrop, TempExtensionRoot, compile_extension},
};
use crate::{
	abi::{
		format_object_for_print, pon_call, pon_const_int, pon_const_str, pon_runtime_init,
		str_::pon_const_bytes,
	},
	import::{module_attr, reset_import_state_for_tests},
	intern::intern,
	thread_state::{pon_err_clear, pon_err_message, test_state_lock},
};

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
		let function =
			module_attr(module_name, intern(name)).unwrap_or_else(|| panic!("{name} registered"));
		let result = unsafe { pon_call(function, ptr::null_mut(), 0) };
		assert!(!result.is_null(), "{name} returned NULL: {:?}", pon_err_message());
		result
	};
	let call_with_args = |name: &str, argv: &mut [*mut crate::object::PyObject]| {
		let function =
			module_attr(module_name, intern(name)).unwrap_or_else(|| panic!("{name} registered"));
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
	assert!(
		result.is_null(),
		"arity_two unexpectedly succeeded: {:?}",
		format_object_for_print(result)
	);
	assert_eq!(
		pon_err_message().as_deref(),
		Some("TypeError: arity_two() takes exactly 2 arguments (1 given)"),
		"PyArg_ParseTuple arity TypeError must match python3.14 getargs.c"
	);
}

#[test]
fn c_api_new_refs_survive_collect_and_decref_unpins() {
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
		"capi_refpin_ext",
		r#"
#include <Python.h>

#define BIT(n) (1L << (n))

static PyObject *exercise(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    long ok = 0;
    const long expected = (long)(1ULL << 62);
    PyObject *value = PyLong_FromLong(expected);
    if (value == NULL) {
        return NULL;
    }

    Py_ssize_t pinned = _PyPon_TestCollectPinCount(value);
    if (pinned == 1) {
        ok |= BIT(0);
    } else if (pinned < 0) {
        PyErr_Clear();
    }

    long observed = PyLong_AsLong(value);
    if ((observed != -1 || PyErr_Occurred() == NULL) && observed == expected) {
        ok |= BIT(1);
    } else if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }

    Py_DECREF(value);
    Py_ssize_t after_decref = _PyPon_TestCollectPinCount(value);
    if (after_decref == 0) {
        ok |= BIT(2);
    } else if (after_decref < 0) {
        PyErr_Clear();
    }

    return PyLong_FromLong(ok);
}

static PyMethodDef methods[] = {
    {"exercise", exercise, METH_NOARGS, "exercise C-API new-reference pins"},
    {NULL, NULL, 0, NULL}
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "capi_refpin_ext",
    "Pon C-API new-reference pin test extension",
    -1,
    methods
};

PyMODINIT_FUNC PyInit_capi_refpin_ext(void) {
    return PyModule_Create(&module);
}
"#,
	);

	let module = load_extension_module("capi_refpin_ext", &module_path)
		.unwrap_or_else(|message| panic!("failed to load refpin C extension: {message}"));
	assert!(!module.is_null(), "extension loader returned NULL module");
	let module_name = intern("capi_refpin_ext");
	let function = module_attr(module_name, intern("exercise")).expect("exercise registered");
	let result = unsafe { pon_call(function, ptr::null_mut(), 0) };
	assert!(!result.is_null(), "exercise() returned NULL: {:?}", pon_err_message());
	assert_eq!(format_object_for_print(result).as_deref(), Ok("7"));
}

#[test]
fn gc_tracked_c_type_traverse_roots_payload() {
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
		"capi_gc_traverse_ext",
		r#"
#include <Python.h>
#include <structmember.h>

#define BIT(n) (1L << (n))

typedef struct {
    PyObject_HEAD
    PyObject *payload;
    PyObject *type_payload;
    PyObject *dict;
} CounterObject;

static long traverse_count = 0;

static int Counter_traverse(CounterObject *self, visitproc visit, void *arg) {
    traverse_count += 1;
    Py_VISIT(self->payload);
    Py_VISIT(self->type_payload);
    return 0;
}

static void Counter_dealloc(CounterObject *self) {
    PyObject_GC_UnTrack(self);
    Py_CLEAR(self->payload);
    Py_CLEAR(self->dict);
    Py_TYPE(self)->tp_free((PyObject *)self);
}

static PyMemberDef Counter_members[] = {
    {"type_payload", T_OBJECT, offsetof(CounterObject, type_payload), READONLY, "foreign type face payload"},
    {"__dict__", T_OBJECT, offsetof(CounterObject, dict), READONLY, "instance dictionary"},
    {NULL, 0, 0, 0, NULL}
};

static PyTypeObject CounterType = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "capi_gc_traverse_ext.Counter",
    .tp_basicsize = sizeof(CounterObject),
    .tp_dealloc = (destructor)Counter_dealloc,
    .tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC,
    .tp_traverse = (traverseproc)Counter_traverse,
    .tp_members = Counter_members,
    .tp_dictoffset = offsetof(CounterObject, dict),
    .tp_new = PyType_GenericNew,
};

static PyObject *drive(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    long ok = 0;
    const long expected = 4611686018427387904L;

    if (PyType_Ready(&CounterType) == 0) {
        ok |= BIT(0);
    } else if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }

    PyObject *obj = PyObject_CallNoArgs((PyObject *)&CounterType);
    if (obj == NULL) {
        return NULL;
    }
    ok |= BIT(1);
    PyObject_GC_Track(obj);

    PyObject *payload = PyLong_FromLong(expected);
    if (payload == NULL) {
        Py_DECREF(obj);
        return NULL;
    }
    ((CounterObject *)obj)->payload = payload;
    ((CounterObject *)obj)->type_payload = (PyObject *)&CounterType;
    PyObject *type_payload = PyObject_GetAttrString(obj, "type_payload");
    if (type_payload == (PyObject *)&CounterType) {
        ok |= BIT(8);
    }
    Py_XDECREF(type_payload);
    if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }
    Py_DECREF(payload);

    PyObject *attr_value = PyLong_FromLong(5);
    if (attr_value == NULL) {
        Py_DECREF(obj);
        return NULL;
    }
    if (PyObject_SetAttrString(obj, "attr", attr_value) == 0) {
        ok |= BIT(9);
    } else if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }
    Py_DECREF(attr_value);

    PyObject *attr_before = PyObject_GetAttrString(obj, "attr");
    if (attr_before != NULL && PyLong_AsLong(attr_before) == 5 && PyErr_Occurred() == NULL) {
        ok |= BIT(10);
    }
    Py_XDECREF(attr_before);
    if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }

    Py_ssize_t attr_collect_a = _PyPon_TestCollectPinCount(NULL);
    Py_ssize_t attr_collect_b = _PyPon_TestCollectPinCount(NULL);
    Py_ssize_t attr_collect_c = _PyPon_TestCollectPinCount(NULL);
    if (attr_collect_a >= 0 && attr_collect_b >= 0 && attr_collect_c >= 0) {
        ok |= BIT(11);
    } else if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }
    PyObject *attr_after = PyObject_GetAttrString(obj, "attr");
    if (attr_after != NULL && PyLong_AsLong(attr_after) == 5 && PyErr_Occurred() == NULL) {
        ok |= BIT(12);
    }
    Py_XDECREF(attr_after);
    if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }

    if (PyObject_SetAttrString(obj, "attr", NULL) == 0) {
        ok |= BIT(13);
    } else if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }
    PyObject *deleted = PyObject_GetAttrString(obj, "attr");
    if (deleted == NULL && PyErr_Occurred() != NULL) {
        PyErr_Clear();
        ok |= BIT(14);
    }
    Py_XDECREF(deleted);

    long before = traverse_count;
    Py_ssize_t first_pin_count = _PyPon_TestCollectPinCount(payload);
    if (first_pin_count == 0) {
        ok |= BIT(2);
    } else if (first_pin_count < 0 && PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }
    Py_ssize_t second_pin_count = _PyPon_TestCollectPinCount(payload);
    if (second_pin_count == 0) {
        ok |= BIT(3);
    } else if (second_pin_count < 0 && PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }
    if (traverse_count >= before + 2) {
        ok |= BIT(4);
    }

    PyObject *survivor = ((CounterObject *)obj)->payload;
    long observed = PyLong_AsLong(survivor);
    if ((observed != -1 || PyErr_Occurred() == NULL) && observed == expected) {
        ok |= BIT(5);
    } else if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }

    PyObject_GC_UnTrack(obj);
    PyObject_GC_Track(obj);
    Py_DECREF(obj);
    Py_ssize_t collect_a = _PyPon_TestCollectPinCount(NULL);
    Py_ssize_t collect_b = _PyPon_TestCollectPinCount(NULL);
    Py_ssize_t collect_c = _PyPon_TestCollectPinCount(NULL);
    if (collect_a >= 0 && collect_b >= 0 && collect_c >= 0) {
        ok |= BIT(6);
    } else if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }
    if (CounterType.tp_flags & Py_TPFLAGS_HAVE_GC) {
        ok |= BIT(7);
    }

    return PyLong_FromLong(ok);
}

static PyMethodDef methods[] = {
    {"drive", drive, METH_NOARGS, "exercise GC-tracked C type traversal"},
    {NULL, NULL, 0, NULL}
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "capi_gc_traverse_ext",
    "Pon C-API GC traversal bridge test extension",
    -1,
    methods
};

PyMODINIT_FUNC PyInit_capi_gc_traverse_ext(void) {
    if (PyType_Ready(&CounterType) < 0) {
        return NULL;
    }
    PyObject *m = PyModule_Create(&module);
    if (m == NULL) {
        return NULL;
    }
    Py_INCREF(&CounterType);
    if (PyModule_AddObject(m, "Counter", (PyObject *)&CounterType) < 0) {
        Py_DECREF(&CounterType);
        Py_DECREF(m);
        return NULL;
    }
    return m;
}
"#,
	);

	let module = load_extension_module("capi_gc_traverse_ext", &module_path)
		.unwrap_or_else(|message| panic!("failed to load GC traversal C extension: {message}"));
	assert!(!module.is_null(), "extension loader returned NULL module");
	let module_name = intern("capi_gc_traverse_ext");
	let function = module_attr(module_name, intern("drive")).expect("drive registered");
	let result = unsafe { pon_call(function, ptr::null_mut(), 0) };
	assert!(!result.is_null(), "drive() returned NULL: {:?}", pon_err_message());
	assert_eq!(format_object_for_print(result).as_deref(), Ok("32767"));
}

#[test]
fn c_api_str_subclass_layout_trailing_fields_survive_unicode_probes() {
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
		"capi_str_subclass_layout_ext",
		r#"
#include <Python.h>
#include <string.h>

#define BIT(n) (1L << (n))

typedef struct {
    PyUnicodeObject base;
    long tag;
} StrSub;

static PyObject *StrSub_new(PyTypeObject *type, PyObject *args, PyObject *kwds) {
    (void)args;
    (void)kwds;
    PyObject *obj = type->tp_alloc(type, 0);
    if (obj != NULL) {
        ((StrSub *)obj)->tag = 0;
    }
    return obj;
}

static void StrSub_dealloc(StrSub *self) {
    Py_TYPE(self)->tp_free((PyObject *)self);
}

static PyTypeObject StrSubType = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "capi_str_subclass_layout_ext.StrSub",
    .tp_basicsize = sizeof(StrSub),
    .tp_dealloc = (destructor)StrSub_dealloc,
    .tp_flags = Py_TPFLAGS_DEFAULT,
    .tp_base = &PyUnicode_Type,
    .tp_new = StrSub_new,
};

static int clear_loud_type_error(void) {
    if (PyErr_ExceptionMatches(PyExc_TypeError)) {
        PyErr_Clear();
        return 1;
    }
    if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }
    return 0;
}

static int sane_unicode_text(PyObject *value) {
    Py_ssize_t length = PyUnicode_GetLength(value);
    if (length < 0) {
        if (PyErr_Occurred() != NULL) {
            PyErr_Clear();
        }
        return 0;
    }
    Py_ssize_t utf8_size = 0;
    const char *text = PyUnicode_AsUTF8AndSize(value, &utf8_size);
    if (text == NULL) {
        if (PyErr_Occurred() != NULL) {
            PyErr_Clear();
        }
        return 0;
    }
    return utf8_size >= 0 && utf8_size < 256;
}

static int str_probe_is_sane_or_loud(PyObject *obj) {
    PyObject *text = PyObject_Str(obj);
    if (text == NULL) {
        return clear_loud_type_error();
    }
    int ok = sane_unicode_text(text);
    Py_DECREF(text);
    return ok;
}

static int unicode_probe_is_sane_or_loud(PyObject *obj) {
    Py_ssize_t length = PyUnicode_GetLength(obj);
    if (length < 0) {
        if (!clear_loud_type_error()) {
            return 0;
        }
    } else if (length >= 256) {
        return 0;
    }

    Py_ssize_t utf8_size = 0;
    const char *text = PyUnicode_AsUTF8AndSize(obj, &utf8_size);
    if (text == NULL) {
        return clear_loud_type_error();
    }
    return utf8_size >= 0 && utf8_size < 256;
}

static PyObject *drive(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    long ok = 0;
    const long expected = 0x1234567L;

    if (PyType_Ready(&StrSubType) == 0) {
        ok |= BIT(0);
    } else if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }

    PyObject *before = PyUnicode_FromString("before-neighbor");
    if (before == NULL) {
        return NULL;
    }

    PyObject *obj = PyObject_CallNoArgs((PyObject *)&StrSubType);
    if (obj == NULL) {
        Py_DECREF(before);
        if (PyErr_Occurred() != NULL) {
            PyErr_Clear();
        }
        return PyLong_FromLong(ok);
    }
    ok |= BIT(1);

    ((StrSub *)obj)->tag = expected;
    if (((StrSub *)obj)->tag == expected) {
        ok |= BIT(2);
    }

    PyObject *after = PyUnicode_FromString("after-neighbor");
    if (after == NULL) {
        Py_DECREF(obj);
        Py_DECREF(before);
        return NULL;
    }

    Py_ssize_t collect_a = _PyPon_TestCollectPinCount(NULL);
    Py_ssize_t collect_b = _PyPon_TestCollectPinCount(NULL);
    Py_ssize_t collect_c = _PyPon_TestCollectPinCount(NULL);
    const char *before_text = PyUnicode_AsUTF8(before);
    const char *after_text = PyUnicode_AsUTF8(after);
    if (collect_a >= 0 && collect_b >= 0 && collect_c >= 0
            && ((StrSub *)obj)->tag == expected
            && before_text != NULL && strcmp(before_text, "before-neighbor") == 0
            && after_text != NULL && strcmp(after_text, "after-neighbor") == 0) {
        ok |= BIT(3);
    } else if (PyErr_Occurred() != NULL) {
        PyErr_Clear();
    }

    if (str_probe_is_sane_or_loud(obj)) {
        ok |= BIT(4);
    }
    if (unicode_probe_is_sane_or_loud(obj)) {
        ok |= BIT(5);
    }

    Py_DECREF(after);
    Py_DECREF(obj);
    Py_DECREF(before);
    return PyLong_FromLong(ok);
}

static PyMethodDef methods[] = {
    {"drive", drive, METH_NOARGS, "exercise str-subclass layout with trailing C fields"},
    {NULL, NULL, 0, NULL}
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT,
    "capi_str_subclass_layout_ext",
    "Pon C-API str-subclass layout regression extension",
    -1,
    methods
};

PyMODINIT_FUNC PyInit_capi_str_subclass_layout_ext(void) {
    return PyModule_Create(&module);
}
"#,
	);

	let module = load_extension_module("capi_str_subclass_layout_ext", &module_path).unwrap_or_else(
		|message| panic!("failed to load str-subclass layout C extension: {message}"),
	);
	assert!(!module.is_null(), "extension loader returned NULL module");
	let module_name = intern("capi_str_subclass_layout_ext");
	let function = module_attr(module_name, intern("drive")).expect("drive registered");
	let result = unsafe { pon_call(function, ptr::null_mut(), 0) };
	assert!(!result.is_null(), "drive() returned NULL: {:?}", pon_err_message());
	assert_eq!(format_object_for_print(result).as_deref(), Ok("63"));
}
