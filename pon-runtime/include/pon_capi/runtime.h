#ifndef PON_CAPI_RUNTIME_H
#define PON_CAPI_RUNTIME_H

/* Runtime family: CPython process services that do not belong to object,
 * number, string, or error protocol families.
 *
 * Pon has no GIL to release. The GIL/thread-state calls therefore expose
 * honest non-NULL tokens and no-op restore/release operations so C extensions
 * can keep their CPython bracketing structure without implying mutual
 * exclusion. PyCapsule destructors are stored for layout compatibility but are
 * not called; capsule objects are process-lifetime in this shim.
 */

typedef struct _ts PyThreadState;
typedef int PyGILState_STATE;
#define PyGILState_LOCKED 0
#define PyGILState_UNLOCKED 1

typedef void (*PyCapsule_Destructor)(PyObject *);

typedef struct PyPonCapiRuntime {
    PyThreadState *(*eval_save_thread)(void);
    void (*eval_restore_thread)(PyThreadState *);
    PyObject *(*capsule_new)(void *, const char *, PyCapsule_Destructor);
    void *(*capsule_get_pointer)(PyObject *, const char *);
    int (*capsule_is_valid)(PyObject *, const char *);
    int (*capsule_set_context)(PyObject *, void *);
    void *(*capsule_get_context)(PyObject *);
    void *(*capsule_import)(const char *, int);
    PyObject *(*import_import_module)(const char *);
    PyObject *(*import_add_module)(const char *);
    PyObject *(*module_get_dict)(PyObject *);
    void *(*module_get_state)(PyObject *);
    const char *(*module_get_name)(PyObject *);
    PyObject *(*sys_get_object)(const char *);
    /* Family expansion point: append fields only; never reorder. */
} PyPonCapiRuntime;

#endif /* PON_CAPI_RUNTIME_H */
