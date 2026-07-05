#ifndef PON_CAPI_OBJECT_INLINE_H
#define PON_CAPI_OBJECT_INLINE_H

/* Inline wrapper layer for the object family. Included by Python.h after the
 * PyPonCapi definition and core/error inline wrappers. */


#define Py_PRINT_RAW 1

static inline int PyType_Check(PyObject *object) {
    return PyPon_Capi()->object_->type_check(object);
}

static inline int PyIter_Check(PyObject *object) {
    return PyPon_Capi()->object_->iter_check(object);
}
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

static inline PyObject *PyObject_CallMethodNoArgs(PyObject *object, PyObject *name) {
    PyObject *method = PyObject_GetAttr(object, name);
    if (method == NULL) {
        return NULL;
    }
    PyObject *result = PyObject_CallNoArgs(method);
    Py_DECREF(method);
    return result;
}

static inline PyObject *PyObject_CallMethodOneArg(PyObject *object, PyObject *name, PyObject *arg) {
    PyObject *method = PyObject_GetAttr(object, name);
    if (method == NULL) {
        return NULL;
    }
    PyObject *result = PyObject_CallOneArg(method, arg);
    Py_DECREF(method);
    return result;
}

static inline PyObject *PyObject_Vectorcall(PyObject *callable, PyObject *const *args, size_t nargsf, PyObject *kwnames) {
    return PyPon_Capi()->object_->vectorcall(callable, args, nargsf, kwnames);
}

static inline PyObject *PyObject_VectorcallDict(PyObject *callable, PyObject *const *args, size_t nargsf, PyObject *kwargs) {
    return PyPon_Capi()->object_->vectorcall_dict(callable, args, nargsf, kwargs);
}

static inline PyObject *PyVectorcall_Call(PyObject *callable, PyObject *tuple, PyObject *dict) {
    return PyPon_Capi()->object_->vectorcall_call(callable, tuple, dict);
}

static inline vectorcallfunc PyVectorcall_Function(PyObject *callable) {
    return (vectorcallfunc)PyPon_Capi()->object_->vectorcall_function(callable);
}

static inline PyObject *PyObject_VectorcallMethod(PyObject *name, PyObject *const *args, size_t nargsf, PyObject *kwnames) {
    Py_ssize_t nargs = PyVectorcall_NARGS(nargsf);
    if (nargs < 1 || args == NULL) {
        PyErr_SetString(PyExc_TypeError, "PyObject_VectorcallMethod requires a receiver");
        return NULL;
    }
    PyObject *method = PyObject_GetAttr(args[0], name);
    if (method == NULL) {
        return NULL;
    }
    size_t forwarded = ((size_t)(nargs - 1)) | (nargsf & PY_VECTORCALL_ARGUMENTS_OFFSET);
    PyObject *result = PyObject_Vectorcall(method, args + 1, forwarded, kwnames);
    Py_DECREF(method);
    return result;
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


static inline int PyObject_Print(PyObject *object, FILE *file, int flags) {
    return PyPon_Capi()->object_->print(object, file, flags);
}

static inline PyObject *PyObject_Format(PyObject *object, PyObject *format_spec) {
    return PyPon_Capi()->object_->format(object, format_spec);
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

static inline PyObject *PyObject_GenericGetAttr(PyObject *object, PyObject *name) {
    return PyPon_Capi()->object_->generic_get_attr(object, name);
}

static inline int PyObject_GenericSetAttr(PyObject *object, PyObject *name, PyObject *value) {
    return PyPon_Capi()->object_->generic_set_attr(object, name, value);
}

static inline PyObject *PyObject_GenericGetDict(PyObject *object, void *context) {
    return PyPon_Capi()->object_->generic_get_dict(object, context);
}

static inline void PyObject_ClearWeakRefs(PyObject *object) {
    PyPon_Capi()->object_->clear_weakrefs(object);
}

static inline PyObject *PySeqIter_New(PyObject *sequence) {
    return PyPon_Capi()->object_->seq_iter_new(sequence);
}

static inline PyObject *PyMethod_New(PyObject *function, PyObject *self) {
    return PyPon_Capi()->object_->method_new(function, self);
}

static inline int PyObject_GetBuffer(PyObject *object, Py_buffer *view, int flags) {
    return PyPon_Capi()->object_->get_buffer(object, view, flags);
}

static inline int PyObject_CheckBuffer(PyObject *object) {
    return PyPon_Capi()->object_->check_buffer(object);
}

static inline int PyBuffer_FillInfo(Py_buffer *view, PyObject *object, void *buf, Py_ssize_t len, int readonly, int flags) {
    return PyPon_Capi()->object_->buffer_fill_info(view, object, buf, len, readonly, flags);
}

static inline int PyBuffer_IsContiguous(const Py_buffer *view, char order) {
    return PyPon_Capi()->object_->buffer_is_contiguous(view, order);
}

static inline PyObject *PyMemoryView_FromObject(PyObject *object) {
    return PyPon_Capi()->object_->memoryview_from_object(object);
}

static inline PyObject *PyMemoryView_FromBuffer(const Py_buffer *view) {
    return PyPon_Capi()->object_->memoryview_from_buffer(view);
}

static inline int PyMemoryView_Check(PyObject *object) {
    return Py_IS_TYPE(object, &PyMemoryView_Type);
}

#define PyMemoryView_GET_BUFFER(object) (PyPon_Capi()->object_->memoryview_get_buffer((PyObject *)(object)))
#define PyMemoryView_GET_BASE(object) (PyPon_Capi()->object_->memoryview_get_base((PyObject *)(object)))

#endif /* PON_CAPI_OBJECT_INLINE_H */
