#ifndef PON_CAPI_TYPEOBJ_INLINE_H
#define PON_CAPI_TYPEOBJ_INLINE_H

#include <stdlib.h>

static inline int PyType_Ready(PyTypeObject *type) {
    return PyPon_Capi()->typeobj->type_ready(type);
}

static inline PyObject *PyType_GenericAlloc(PyTypeObject *type, Py_ssize_t nitems) {
    return PyPon_Capi()->typeobj->generic_alloc(type, nitems);
}

static inline PyObject *PyType_GenericNew(PyTypeObject *type, PyObject *args, PyObject *kwds) {
    return PyPon_Capi()->typeobj->generic_new(type, args, kwds);
}

static inline PyObject *PyType_FromSpec(PyType_Spec *spec) {
    return PyPon_Capi()->typeobj->type_from_spec(spec);
}

static inline PyObject *PyType_FromSpecWithBases(PyType_Spec *spec, PyObject *bases) {
    return PyPon_Capi()->typeobj->type_from_spec_with_bases(spec, bases);
}

static inline PyObject *PyType_FromModuleAndSpec(PyObject *module, PyType_Spec *spec, PyObject *bases) {
    return PyPon_Capi()->typeobj->type_from_module_and_spec(module, spec, bases);
}

static inline int PyType_IsSubtype(PyTypeObject *a, PyTypeObject *b) {
    return PyPon_Capi()->typeobj->is_subtype(a, b);
}

static inline int PyObject_TypeCheck(PyObject *ob, PyTypeObject *type) {
    PyTypeObject *actual = Py_TYPE(ob);
    return actual == type || PyType_IsSubtype(actual, type);
}

static inline unsigned long PyType_GetFlags(PyTypeObject *type) {
    return (unsigned long)type->tp_flags;
}

static inline int PyType_HasFeature(PyTypeObject *type, unsigned long feature) {
    return (type->tp_flags & feature) != 0;
}

#define PyType_FastSubclass(type, flag) PyType_HasFeature((type), (flag))

/* Object memory: PyObject_Malloc pairs with PyObject_Free for raw blocks;
 * PyObject_Free is also the default tp_free and no-ops for GC-owned
 * instances. */
static inline void *PyObject_Malloc(size_t size) {
    return malloc(size ? size : 1);
}

static inline void PyObject_Free(void *ptr) {
    PyPon_Capi()->typeobj->object_free(ptr);
}

#define PyObject_Del PyObject_Free

static inline PyObject *PyObject_Init(PyObject *op, PyTypeObject *type) {
    return PyPon_Capi()->typeobj->object_init(op, type);
}

static inline PyObject *_PyPon_ObjectNew(PyTypeObject *type, Py_ssize_t nitems) {
    return PyPon_Capi()->typeobj->object_new_raw(type, nitems);
}

#define PyObject_New(type, typeobj) ((type *)_PyPon_ObjectNew((typeobj), 0))
#define PyObject_NewVar(type, typeobj, n) ((type *)_PyPon_ObjectNew((typeobj), (n)))
#define PyObject_GC_New(type, typeobj) PyObject_New(type, typeobj)
#define PyObject_GC_NewVar(type, typeobj, n) PyObject_NewVar(type, typeobj, n)
#define PyObject_GC_Del PyObject_Free

/* GC tracking is implicit under Pon's collector. */
#define PyObject_GC_Track(op) ((void)(op))
#define PyObject_GC_UnTrack(op) ((void)(op))

#endif /* PON_CAPI_TYPEOBJ_INLINE_H */
