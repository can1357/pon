#ifndef PON_CAPI_OBJECT_INLINE_H
#define PON_CAPI_OBJECT_INLINE_H

/* Inline wrapper layer for the object family. Included by Python.h after the
 * PyPonCapi definition and core/error inline wrappers. */

static inline PyObject *PyObject_GetAttr(PyObject *object, PyObject *name) {
    return PyPon_Capi()->object_->get_attr(object, name);
}

static inline PyObject *PyObject_GetAttrString(PyObject *object, const char *name) {
    return PyPon_Capi()->object_->get_attr_string(object, name);
}

static inline int PyObject_GetOptionalAttr(PyObject *object, PyObject *name, PyObject **result) {
    return PyPon_Capi()->object_->get_optional_attr(object, name, result);
}

static inline int PyObject_SetAttr(PyObject *object, PyObject *name, PyObject *value) {
    return PyPon_Capi()->object_->set_attr(object, name, value);
}

static inline int PyObject_SetAttrString(PyObject *object, const char *name, PyObject *value) {
    return PyPon_Capi()->object_->set_attr_string(object, name, value);
}

static inline int PyObject_HasAttr(PyObject *object, PyObject *name) {
    return PyPon_Capi()->object_->has_attr(object, name);
}

static inline int PyObject_HasAttrString(PyObject *object, const char *name) {
    return PyPon_Capi()->object_->has_attr_string(object, name);
}

static inline PyObject *PyObject_Call(PyObject *callable, PyObject *args, PyObject *kwargs) {
    return PyPon_Capi()->object_->call(callable, args, kwargs);
}

static inline PyObject *PyObject_CallObject(PyObject *callable, PyObject *args) {
    return PyPon_Capi()->object_->call_object(callable, args);
}

static inline PyObject *PyObject_CallNoArgs(PyObject *callable) {
    return PyPon_Capi()->object_->call_no_args(callable);
}

static inline PyObject *PyObject_CallOneArg(PyObject *callable, PyObject *arg) {
    return PyPon_Capi()->object_->call_one_arg(callable, arg);
}

static inline PyObject *_PyPon_CallArgsFromFormat(const char *format, va_list vargs) {
    if (format == NULL || format[0] == '\0') {
        return PyTuple_New(0);
    }
    PyObject *built = Py_VaBuildValue(format, vargs);
    if (built == NULL) {
        return NULL;
    }
    if (PyTuple_Check(built)) {
        return built;
    }
    PyObject *tuple = PyTuple_New(1);
    if (tuple == NULL) {
        Py_DECREF(built);
        return NULL;
    }
    if (PyTuple_SetItem(tuple, 0, built) < 0) {
        Py_DECREF(tuple);
        Py_DECREF(built);
        return NULL;
    }
    return tuple;
}

static inline PyObject *_PyPon_CallWithFormat(PyObject *callable, const char *format, va_list vargs) {
    if (format == NULL || format[0] == '\0') {
        return PyObject_CallNoArgs(callable);
    }
    PyObject *args = _PyPon_CallArgsFromFormat(format, vargs);
    if (args == NULL) {
        return NULL;
    }
    PyObject *result = PyObject_CallObject(callable, args);
    Py_DECREF(args);
    return result;
}

static inline PyObject *PyObject_CallFunction(PyObject *callable, const char *format, ...) {
    va_list vargs;
    va_start(vargs, format);
    PyObject *result = _PyPon_CallWithFormat(callable, format, vargs);
    va_end(vargs);
    return result;
}

static inline PyObject *PyObject_CallMethod(PyObject *object, const char *name, const char *format, ...) {
    if (name == NULL) {
        PyErr_SetString(PyExc_TypeError, "method name must not be NULL");
        return NULL;
    }
    PyObject *method = PyObject_GetAttrString(object, name);
    if (method == NULL) {
        return NULL;
    }
    va_list vargs;
    va_start(vargs, format);
    PyObject *result = _PyPon_CallWithFormat(method, format, vargs);
    va_end(vargs);
    Py_DECREF(method);
    return result;
}

#define _PON_OBJECT_VARARGS_CAP 16

static inline int _PyPon_CollectObjectVarargs(va_list vargs, PyObject **argv, size_t *argc) {
    PyObject *arg;
    *argc = 0;
    while ((arg = va_arg(vargs, PyObject *)) != NULL) {
        if (*argc == _PON_OBJECT_VARARGS_CAP) {
            return -1;
        }
        argv[(*argc)++] = arg;
    }
    return 0;
}

static inline PyObject *PyObject_CallFunctionObjArgs(PyObject *callable, ...) {
    PyObject *argv[_PON_OBJECT_VARARGS_CAP];
    size_t argc;
    va_list vargs;
    va_start(vargs, callable);
    int status = _PyPon_CollectObjectVarargs(vargs, argv, &argc);
    va_end(vargs);
    if (status < 0) {
        PyErr_SetString(PyExc_TypeError, "Pon PyObject_CallFunctionObjArgs supports at most 16 arguments");
        return NULL;
    }
    return PyPon_Capi()->object_->call_varargs(callable, NULL, argv, argc);
}

static inline PyObject *PyObject_CallMethodObjArgs(PyObject *object, PyObject *name, ...) {
    PyObject *argv[_PON_OBJECT_VARARGS_CAP];
    size_t argc;
    va_list vargs;
    va_start(vargs, name);
    int status = _PyPon_CollectObjectVarargs(vargs, argv, &argc);
    va_end(vargs);
    if (status < 0) {
        PyErr_SetString(PyExc_TypeError, "Pon PyObject_CallMethodObjArgs supports at most 16 arguments");
        return NULL;
    }
    return PyPon_Capi()->object_->call_varargs(object, name, argv, argc);
}

static inline PyObject *PyObject_Repr(PyObject *object) {
    return PyPon_Capi()->object_->repr(object);
}

static inline PyObject *PyObject_Str(PyObject *object) {
    return PyPon_Capi()->object_->str(object);
}

static inline int PyObject_IsTrue(PyObject *object) {
    return PyPon_Capi()->object_->is_true(object);
}

static inline int PyObject_Not(PyObject *object) {
    return PyPon_Capi()->object_->not_(object);
}

static inline PyObject *PyObject_RichCompare(PyObject *left, PyObject *right, int op) {
    return PyPon_Capi()->object_->rich_compare(left, right, op);
}

static inline int PyObject_RichCompareBool(PyObject *left, PyObject *right, int op) {
    return PyPon_Capi()->object_->rich_compare_bool(left, right, op);
}

static inline PyObject *PyObject_GetItem(PyObject *object, PyObject *key) {
    return PyPon_Capi()->object_->get_item(object, key);
}

static inline int PyObject_SetItem(PyObject *object, PyObject *key, PyObject *value) {
    return PyPon_Capi()->object_->set_item(object, key, value);
}

static inline int PyObject_DelItem(PyObject *object, PyObject *key) {
    return PyPon_Capi()->object_->del_item(object, key);
}

static inline PyObject *PyObject_GetIter(PyObject *object) {
    return PyPon_Capi()->object_->get_iter(object);
}

static inline PyObject *PyIter_Next(PyObject *iterator) {
    return PyPon_Capi()->object_->iter_next(iterator);
}

static inline Py_ssize_t PyObject_Size(PyObject *object) {
    return PyPon_Capi()->object_->size(object);
}

#define PyObject_Length PyObject_Size

static inline Py_hash_t PyObject_Hash(PyObject *object) {
    return PyPon_Capi()->object_->hash(object);
}

static inline int PyObject_AsFileDescriptor(PyObject *object) {
    return PyPon_Capi()->object_->as_file_descriptor(object);
}

static inline int PyCallable_Check(PyObject *object) {
    return PyPon_Capi()->object_->callable_check(object);
}

static inline int PyObject_IsInstance(PyObject *object, PyObject *classinfo) {
    return PyPon_Capi()->object_->is_instance(object, classinfo);
}

static inline int PyObject_IsSubclass(PyObject *object, PyObject *classinfo) {
    return PyPon_Capi()->object_->is_subclass(object, classinfo);
}

static inline PyObject *PyObject_Type(PyObject *object) {
    return PyPon_Capi()->object_->type(object);
}

static inline PyObject *PyObject_SelfIter(PyObject *object) {
    return PyPon_Capi()->object_->self_iter(object);
}

#endif /* PON_CAPI_OBJECT_INLINE_H */
