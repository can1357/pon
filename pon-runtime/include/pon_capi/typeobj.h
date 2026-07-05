#ifndef PON_CAPI_TYPEOBJ_H
#define PON_CAPI_TYPEOBJ_H

/* typeobj family: PyType_Ready and C-defined type instantiation.
 *
 * Supported in this iteration: static PyTypeObject definitions with
 * tp_new/tp_init/tp_dealloc, tp_methods/tp_getset/tp_members, tp_doc, the
 * object-protocol slots (repr/str/hash/call/richcompare/iter/getattro/...),
 * and single inheritance from `object`, builtin twins, or other Ready'd
 * foreign types.
 *
 * NOT supported (PyType_Ready fails loudly, never silently):
 * - Py_TPFLAGS_HAVE_GC, tp_traverse, tp_clear (GC-tracked C types),
 * - custom metatypes (ob_type must be NULL or resolve to `type`).
 *
 * tp_dealloc bridging: Pon's GC runs tp_dealloc as a deferred finalizer —
 * the object and everything it references stay valid for the whole callback;
 * the memory block is reclaimed one collection cycle later. A dealloc that
 * resurrects the object after tearing down its own payload produces a
 * valid-but-torn-down object, exactly as on CPython. tp_free on GC-owned
 * instances is a no-op (the collector owns the block).
 *
 * TODO(typeobj): type->tp_dict stays NULL after PyType_Ready — the native
 * class dict cannot cross the boundary as a dict object yet. Mutate type
 * attributes through PyObject_SetAttrString((PyObject *)type, ...) instead;
 * PyDict_* on a NULL tp_dict raises rather than crashing.
 */

typedef struct PyPonCapiTypeObj {
    int (*type_ready)(PyTypeObject *);
    PyObject *(*generic_alloc)(PyTypeObject *, Py_ssize_t);
    PyObject *(*generic_new)(PyTypeObject *, PyObject *, PyObject *);
    int (*is_subtype)(PyTypeObject *, PyTypeObject *);
    void (*object_free)(void *);
    PyObject *(*object_init)(PyObject *, PyTypeObject *);
    PyObject *(*object_new_raw)(PyTypeObject *, Py_ssize_t);
    PyObject *(*type_from_spec)(PyType_Spec *);
    PyObject *(*type_from_spec_with_bases)(PyType_Spec *, PyObject *);
    PyObject *(*type_from_module_and_spec)(PyObject *, PyType_Spec *, PyObject *);
    /* Family expansion point: append fields only; never reorder. */
} PyPonCapiTypeObj;

#endif /* PON_CAPI_TYPEOBJ_H */
