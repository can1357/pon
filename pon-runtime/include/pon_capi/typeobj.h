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
 * GC-tracked C types are accepted: Py_TPFLAGS_HAVE_GC does not change
 * allocation ownership under Pon, tp_traverse is bridged into the runtime
 * tracer, and tp_clear is intentionally ignored because Pon's tracing GC does
 * not need C-level cycle breaking.
 *
 * tp_dealloc bridging: Pon's GC runs tp_dealloc as a deferred finalizer —
 * the object and everything it references stay valid for the whole callback;
 * the memory block is reclaimed one collection cycle later. A dealloc that
 * resurrects the object after tearing down its own payload produces a
 * valid-but-torn-down object, exactly as on CPython. tp_free on GC-owned
 * instances is a no-op (the collector owns the block).
 *
 * PyType_Ready backfills type->tp_dict with a live dict-shaped view over the
 * native class namespace. PyDict_GetItem* reads current class entries, and
 * PyDict_SetItem* writes update native type attributes and invalidate type
 * caches just like PyObject_SetAttrString((PyObject *)type, ...).
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
    void (*type_modified)(PyTypeObject *);
    PyObject *(*type_from_metaclass)(PyTypeObject *, PyObject *, PyType_Spec *, PyObject *);
    /* Family expansion point: append fields only; never reorder. */
} PyPonCapiTypeObj;

#endif /* PON_CAPI_TYPEOBJ_H */
