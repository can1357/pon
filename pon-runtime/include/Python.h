#ifndef PON_PYTHON_H
#define PON_PYTHON_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef intptr_t Py_ssize_t;
typedef struct _typeobject PyTypeObject;

typedef struct _object {
    PyTypeObject *ob_type;
    uintptr_t gc_meta;
} PyObject;

#define PyObject_HEAD PyObject ob_base;
#define PyObject_HEAD_INIT(type) { (type), 0 }

typedef PyObject *(*PyCFunction)(PyObject *, PyObject *);

typedef struct PyMethodDef {
    const char *ml_name;
    PyCFunction ml_meth;
    int ml_flags;
    const char *ml_doc;
} PyMethodDef;

typedef struct PyModuleDef_Base {
    PyObject ob_base;
    void *m_init;
    Py_ssize_t m_index;
    PyObject *m_copy;
} PyModuleDef_Base;

typedef struct PyModuleDef {
    PyModuleDef_Base m_base;
    const char *m_name;
    const char *m_doc;
    Py_ssize_t m_size;
    PyMethodDef *m_methods;
    void *m_slots;
    void *m_traverse;
    void *m_clear;
    void *m_free;
} PyModuleDef;

#define PyModuleDef_HEAD_INIT { PyObject_HEAD_INIT(NULL), NULL, 0, NULL }
#define PYTHON_API_VERSION 1013

#define METH_VARARGS 0x0001
#define METH_KEYWORDS 0x0002
#define METH_NOARGS 0x0004
#define METH_O 0x0008
#define METH_CLASS 0x0010
#define METH_STATIC 0x0020
#define METH_COEXIST 0x0040
#define METH_FASTCALL 0x0080
#define METH_METHOD 0x0200

#ifndef PyMODINIT_FUNC
#define PyMODINIT_FUNC PyObject *
#endif

typedef struct PyPonCapi {
    PyObject *(*module_create2)(PyModuleDef *, int);
    int (*module_add_object)(PyObject *, const char *, PyObject *);
    PyObject *(*long_from_long)(long);
    long (*long_as_long)(PyObject *);
    PyObject *(*unicode_from_string)(const char *);
    void (*inc_ref)(PyObject *);
    void (*dec_ref)(PyObject *);
    PyObject *(*none)(void);
    void (*err_set_string)(PyObject *, const char *);
    PyObject *(*err_occurred)(void);
    PyObject *exc_runtime_error;
    PyObject *exc_type_error;
    PyObject *exc_value_error;
    PyObject *exc_import_error;
} PyPonCapi;

int PyPon_SetCapi(const PyPonCapi *api);
const PyPonCapi *PyPon_GetCapi(void);

static inline const PyPonCapi *PyPon_Capi(void) {
    return PyPon_GetCapi();
}

static inline PyObject *PyModule_Create2(PyModuleDef *module, int api_version) {
    return PyPon_Capi()->module_create2(module, api_version);
}

#define PyModule_Create(module) PyModule_Create2((module), PYTHON_API_VERSION)

static inline int PyModule_AddObject(PyObject *module, const char *name, PyObject *value) {
    return PyPon_Capi()->module_add_object(module, name, value);
}

static inline int PyModule_AddObjectRef(PyObject *module, const char *name, PyObject *value) {
    PyPon_Capi()->inc_ref(value);
    return PyPon_Capi()->module_add_object(module, name, value);
}

static inline PyObject *PyLong_FromLong(long value) {
    return PyPon_Capi()->long_from_long(value);
}

static inline long PyLong_AsLong(PyObject *object) {
    return PyPon_Capi()->long_as_long(object);
}

static inline PyObject *PyUnicode_FromString(const char *value) {
    return PyPon_Capi()->unicode_from_string(value);
}

static inline void Py_IncRef(PyObject *object) {
    PyPon_Capi()->inc_ref(object);
}

static inline void Py_DecRef(PyObject *object) {
    PyPon_Capi()->dec_ref(object);
}

#define Py_INCREF(op) Py_IncRef((PyObject *)(op))
#define Py_DECREF(op) Py_DecRef((PyObject *)(op))
#define Py_XINCREF(op) do { if ((op) != NULL) Py_INCREF(op); } while (0)
#define Py_XDECREF(op) do { if ((op) != NULL) Py_DECREF(op); } while (0)

#define Py_None (PyPon_Capi()->none())
#define Py_RETURN_NONE do { Py_INCREF(Py_None); return Py_None; } while (0)

#define PyExc_RuntimeError (PyPon_Capi()->exc_runtime_error)
#define PyExc_TypeError (PyPon_Capi()->exc_type_error)
#define PyExc_ValueError (PyPon_Capi()->exc_value_error)
#define PyExc_ImportError (PyPon_Capi()->exc_import_error)

static inline void PyErr_SetString(PyObject *exception, const char *message) {
    PyPon_Capi()->err_set_string(exception, message);
}

static inline PyObject *PyErr_Occurred(void) {
    return PyPon_Capi()->err_occurred();
}

#ifdef __cplusplus
}
#endif

#endif /* PON_PYTHON_H */
