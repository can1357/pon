#ifndef PON_CAPI_CORE_H
#define PON_CAPI_CORE_H

/* Core family: module creation, reference counting, singletons, and the
 * foreign-twin type machinery. See Python.h for the identity contract.
 *
 * Builtin type ids: indexes into the bootstrap's local twin array. Order is
 * FROZEN; append only.
 */

#define PON_TID_TYPE 0
#define PON_TID_OBJECT 1
#define PON_TID_LONG 2
#define PON_TID_BOOL 3
#define PON_TID_FLOAT 4
#define PON_TID_COMPLEX 5
#define PON_TID_UNICODE 6
#define PON_TID_BYTES 7
#define PON_TID_BYTEARRAY 8
#define PON_TID_TUPLE 9
#define PON_TID_LIST 10
#define PON_TID_DICT 11
#define PON_TID_SET 12
#define PON_TID_FROZENSET 13
#define PON_TID_SLICE 14
#define PON_TID_MEMORYVIEW 15
#define PON_TID_CAPSULE 16
#define PON_TID_NONE_TYPE 17
#define PON_BUILTIN_TYPE_COUNT 18

typedef struct PyPonCapiCore {
    PyObject *(*module_create2)(PyModuleDef *, int);
    int (*module_add_object)(PyObject *, const char *, PyObject *);
    void (*inc_ref)(PyObject *);
    void (*dec_ref)(PyObject *);
    PyObject *(*none)(void);
    PyObject *(*bool_true)(void);
    PyObject *(*bool_false)(void);
    PyObject *(*not_implemented)(void);

    /* Twin machinery.
     * register_local_twins: called once from PyPon_SetCapi with the
     * bootstrap's local twin globals; the runtime fills each struct's
     * descriptive fields and records address->native mappings.
     * builtin_type_id: PON_TID_* for the object's runtime type, or -1.
     * foreign_of: canonical foreign twin for the object's runtime type
     * (an extension's own static once PyType_Ready registered it). */
    int (*register_local_twins)(PyTypeObject *const *, int);
    int (*builtin_type_id)(PyObject *);
    PyTypeObject *(*foreign_of)(PyObject *);
} PyPonCapiCore;

#endif /* PON_CAPI_CORE_H */
