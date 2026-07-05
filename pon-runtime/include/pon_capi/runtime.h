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

typedef struct _is PyInterpreterState;
/* Minimal thread-state facade: NumPy still reads `tstate->interp`, while the
 * interpreter object and all other thread state remain opaque process
 * singletons owned by Pon.
 */
typedef struct _ts {
    PyInterpreterState *interp;
} PyThreadState;
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
    PyObject *(*module_def_init)(PyModuleDef *);
    PyThreadState *(*thread_state_get)(void);
    PyFrameObject *(*thread_state_get_frame)(PyThreadState *);
    PyInterpreterState *(*interpreter_state_main)(void);
    PyObject *(*eval_get_builtins)(void);
    PyFrameObject *(*frame_get_back)(PyFrameObject *);
    PyCodeObject *(*frame_get_code)(PyFrameObject *);
    PyObject *(*contextvar_new)(const char *, PyObject *);
    int (*contextvar_get)(PyObject *, PyObject *, PyObject **);
    void *(*datetime_capi_import)(void);
    int (*datetime_get_attr_int)(PyObject *, const char *);
    int (*capsule_set_name)(PyObject *, const char *);
    PyObject *(*import_import)(PyObject *);
#ifdef PON_CAPI_TESTING
    Py_ssize_t (*test_collect_pin_count)(PyObject *);
#endif
    /* Family expansion point: append fields only; never reorder. */
} PyPonCapiRuntime;

#endif /* PON_CAPI_RUNTIME_H */
