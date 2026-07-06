#ifndef PON_CAPI_CORE_INLINE_H
#define PON_CAPI_CORE_INLINE_H

#include <stdio.h>
#include <stdlib.h>

/* Inline wrapper layer over the family tables. Included by Python.h AFTER
 * the PyPonCapi definition; never include directly.
 *
 * Builtin type names (PyLong_Type, ...) are REAL globals with storage in
 * pon_capi_bootstrap.c, so `&PyLong_Type` is a link-time constant usable in
 * static initializers. PyPon_SetCapi registers them with the runtime, which
 * fills their descriptive fields and maps their addresses at the boundary.
 */

/* ---- bootstrap-local builtin twin globals ---- */

extern PyTypeObject PyType_Type;
extern PyTypeObject PyBaseObject_Type;
extern PyTypeObject PyLong_Type;
extern PyTypeObject PyBool_Type;
extern PyTypeObject PyFloat_Type;
extern PyTypeObject PyComplex_Type;
extern PyTypeObject PyUnicode_Type;
extern PyTypeObject PyBytes_Type;
extern PyTypeObject PyByteArray_Type;
extern PyTypeObject PyTuple_Type;
extern PyTypeObject PyList_Type;
extern PyTypeObject PyDict_Type;
extern PyTypeObject PySet_Type;
extern PyTypeObject PyFrozenSet_Type;
extern PyTypeObject PySlice_Type;
extern PyTypeObject PyMemoryView_Type;
extern PyTypeObject PyCapsule_Type;
extern PyTypeObject _PyNone_Type;
extern PyTypeObject PyRange_Type;

/* Local twin lookup array, indexed by PON_TID_*; storage in the bootstrap. */
extern PyTypeObject *const _PyPon_LocalTwins[PON_BUILTIN_TYPE_COUNT];

/* ---- type identity ---- */

/* Py_TYPE(): localize builtin types to this extension's twin globals so
 * `Py_TYPE(x) == &PyLong_Type` holds; other types resolve to their canonical
 * foreign face (an extension's own static once PyType_Ready registered it). */
static inline PyTypeObject *_PyPon_Type(PyObject *ob) {
    int tid = PyPon_Capi()->core->builtin_type_id(ob);
    if (tid >= 0 && tid < PON_BUILTIN_TYPE_COUNT) {
        return _PyPon_LocalTwins[tid];
    }
    return PyPon_Capi()->core->foreign_of(ob);
}
#define Py_TYPE(ob) _PyPon_Type((PyObject *)(ob))

static inline int Py_IS_TYPE(PyObject *ob, PyTypeObject *type) {
    return Py_TYPE(ob) == type;
}

/* Reference counts are owned by Pon's GC; pinning happens via Py_INCREF.
 * Py_REFCNT reports a conservative >1 so in-place optimizations stay off. */
static inline Py_ssize_t _PyPon_RefCnt(PyObject *ob) {
    (void)ob;
    return 2;
}
#define Py_REFCNT(ob) _PyPon_RefCnt((PyObject *)(ob))

/* ---- reference counting ---- */

static inline void Py_IncRef(PyObject *object) {
    PyPon_Capi()->core->inc_ref(object);
}

static inline void Py_DecRef(PyObject *object) {
    PyPon_Capi()->core->dec_ref(object);
}

#define Py_INCREF(op) Py_IncRef((PyObject *)(op))
#define Py_DECREF(op) Py_DecRef((PyObject *)(op))
#define Py_XINCREF(op) do { if ((op) != NULL) Py_INCREF(op); } while (0)
#define Py_XDECREF(op) do { if ((op) != NULL) Py_DECREF(op); } while (0)
#define Py_CLEAR(op) do { void *_py_tmp = (void *)(op); if (_py_tmp != NULL) { (op) = NULL; Py_DECREF((PyObject *)_py_tmp); } } while (0)
#define Py_SETREF(dst, src) do { void *_py_tmp = (void *)(dst); (dst) = (src); Py_XDECREF((PyObject *)_py_tmp); } while (0)
#define Py_XSETREF(dst, src) Py_SETREF(dst, src)

static inline PyObject *Py_NewRef(PyObject *object) {
    Py_INCREF(object);
    return object;
}

static inline PyObject *Py_XNewRef(PyObject *object) {
    Py_XINCREF(object);
    return object;
}

/* ---- singletons ---- */

#define Py_None (PyPon_Capi()->core->none())
#define Py_True (PyPon_Capi()->core->bool_true())
#define Py_False (PyPon_Capi()->core->bool_false())
#define Py_NotImplemented (PyPon_Capi()->core->not_implemented())

#define Py_RETURN_NONE do { PyObject *_pon_none = Py_None; Py_INCREF(_pon_none); return _pon_none; } while (0)
#define Py_RETURN_TRUE do { PyObject *_pon_true = Py_True; Py_INCREF(_pon_true); return _pon_true; } while (0)
#define Py_RETURN_FALSE do { PyObject *_pon_false = Py_False; Py_INCREF(_pon_false); return _pon_false; } while (0)
#define Py_RETURN_NOTIMPLEMENTED do { PyObject *_pon_ni = Py_NotImplemented; Py_INCREF(_pon_ni); return _pon_ni; } while (0)

/* ---- modules ---- */

static inline PyObject *PyModule_Create2(PyModuleDef *module, int api_version) {
    return PyPon_Capi()->core->module_create2(module, api_version);
}

#define PyModule_Create(module) PyModule_Create2((module), PYTHON_API_VERSION)

static inline int PyModule_AddObject(PyObject *module, const char *name, PyObject *value) {
    return PyPon_Capi()->core->module_add_object(module, name, value);
}

static inline int PyModule_AddObjectRef(PyObject *module, const char *name, PyObject *value) {
    Py_XINCREF(value);
    return PyPon_Capi()->core->module_add_object(module, name, value);
}

static inline int PyModule_AddIntConstant(PyObject *module, const char *name, long value) {
    return PyPon_Capi()->core->module_add_object(module, name, PyPon_Capi()->numbers->long_from_long(value));
}

static inline int PyModule_AddStringConstant(PyObject *module, const char *name, const char *value) {
    return PyPon_Capi()->core->module_add_object(module, name, PyPon_Capi()->strings->unicode_from_string(value));
}


/* ---- errors ---- */

#define PyExc_BaseException (PyPon_Capi()->err->exc_base_exception)
#define PyExc_Exception (PyPon_Capi()->err->exc_exception)
#define PyExc_RuntimeError (PyPon_Capi()->err->exc_runtime_error)
#define PyExc_TypeError (PyPon_Capi()->err->exc_type_error)
#define PyExc_ValueError (PyPon_Capi()->err->exc_value_error)
#define PyExc_ImportError (PyPon_Capi()->err->exc_import_error)
#define PyExc_OverflowError (PyPon_Capi()->err->exc_overflow_error)
#define PyExc_IndexError (PyPon_Capi()->err->exc_index_error)
#define PyExc_KeyError (PyPon_Capi()->err->exc_key_error)
#define PyExc_AttributeError (PyPon_Capi()->err->exc_attribute_error)
#define PyExc_NotImplementedError (PyPon_Capi()->err->exc_not_implemented_error)
#define PyExc_StopIteration (PyPon_Capi()->err->exc_stop_iteration)
#define PyExc_MemoryError (PyPon_Capi()->err->exc_memory_error)
#define PyExc_OSError (PyPon_Capi()->err->exc_os_error)
#define PyExc_SystemError (PyPon_Capi()->err->exc_system_error)
#define PyExc_BufferError (PyPon_Capi()->err->exc_buffer_error)
#define PyExc_ZeroDivisionError (PyPon_Capi()->err->exc_zero_division_error)
#define PyExc_ArithmeticError (PyPon_Capi()->err->exc_arithmetic_error)
#define PyExc_FloatingPointError (PyPon_Capi()->err->exc_floating_point_error)
#define PyExc_DeprecationWarning (PyPon_Capi()->err->exc_deprecation_warning)
#define PyExc_RuntimeWarning (PyPon_Capi()->err->exc_runtime_warning)
#define PyExc_UserWarning (PyPon_Capi()->err->exc_user_warning)
#define PyExc_LookupError (PyPon_Capi()->err->exc_lookup_error)
/* CPython aliases OSError as IOError for legacy spellings. */
#define PyExc_IOError PyExc_OSError

#define Py_Ellipsis (PyPon_Capi()->core->ellipsis())

static inline void PyErr_SetString(PyObject *exception, const char *message) {
    PyPon_Capi()->err->set_string(exception, message);
}

static inline void PyErr_SetObject(PyObject *exception, PyObject *value) {
    PyPon_Capi()->err->set_object(exception, value);
}

static inline void PyErr_SetNone(PyObject *exception) {
    PyPon_Capi()->err->set_none(exception);
}

static inline PyObject *PyErr_Occurred(void) {
    return PyPon_Capi()->err->occurred();
}

static inline void PyErr_Clear(void) {
    PyPon_Capi()->err->clear();
}

static inline PyObject *PyErr_NoMemory(void) {
    PyErr_SetNone(PyExc_MemoryError);
    return NULL;
}

static inline int PyErr_BadArgument(void) {
    PyErr_SetString(PyExc_TypeError, "bad argument type for built-in operation");
    return 0;
}

static inline void PyErr_BadInternalCall(void) {
    PyErr_SetString(PyExc_SystemError, "bad argument to internal function");
}

static inline int PyErr_ExceptionMatches(PyObject *exception) {
    return PyPon_Capi()->err->exception_matches(exception);
}

static inline int PyErr_GivenExceptionMatches(PyObject *given, PyObject *exception) {
    return PyPon_Capi()->err->given_exception_matches(given, exception);
}

static inline void PyErr_Fetch(PyObject **ptype, PyObject **pvalue, PyObject **ptraceback) {
    PyPon_Capi()->err->fetch(ptype, pvalue, ptraceback);
}

static inline void PyErr_Restore(PyObject *type, PyObject *value, PyObject *traceback) {
    PyPon_Capi()->err->restore(type, value, traceback);
}

static inline int PyErr_WarnEx(PyObject *category, const char *message, Py_ssize_t stack_level) {
    return PyPon_Capi()->err->warn_ex(category, message, stack_level);
}

static inline void PyErr_WriteUnraisable(PyObject *object) {
    PyPon_Capi()->err->write_unraisable(object);
}

static inline int PyOS_vsnprintf(char *str, size_t size, const char *format, va_list va) {
    return vsnprintf(str, size, format, va);
}

static inline int PyOS_snprintf(char *str, size_t size, const char *format, ...) {
    va_list va;
    va_start(va, format);
    int result = PyOS_vsnprintf(str, size, format, va);
    va_end(va);
    return result;
}

static inline PyObject *PyErr_FormatV(PyObject *exception, const char *format, va_list vargs) {
    char stack[512];
    va_list copy;
    va_copy(copy, vargs);
    int needed = PyOS_vsnprintf(stack, sizeof(stack), format, copy);
    va_end(copy);
    if (needed < 0) {
        PyErr_SetString(exception, "error formatting C exception message");
        return NULL;
    }
    if ((size_t)needed < sizeof(stack)) {
        PyErr_SetString(exception, stack);
        return NULL;
    }
    char *heap = (char *)malloc((size_t)needed + 1);
    if (heap == NULL) {
        return PyErr_NoMemory();
    }
    va_copy(copy, vargs);
    int written = PyOS_vsnprintf(heap, (size_t)needed + 1, format, copy);
    va_end(copy);
    if (written < 0) {
        free(heap);
        PyErr_SetString(exception, "error formatting C exception message");
        return NULL;
    }
    PyErr_SetString(exception, heap);
    free(heap);
    return NULL;
}

static inline PyObject *PyErr_Format(PyObject *exception, const char *format, ...) {
    va_list va;
    va_start(va, format);
    PyObject *result = PyErr_FormatV(exception, format, va);
    va_end(va);
    return result;
}

#endif /* PON_CAPI_CORE_INLINE_H */
