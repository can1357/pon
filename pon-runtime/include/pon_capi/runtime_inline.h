#ifndef PON_CAPI_RUNTIME_INLINE_H
#define PON_CAPI_RUNTIME_INLINE_H

#include <stdlib.h>

/* Memory APIs are direct libc calls in this source-recompiled shim. CPython
 * promises a distinct non-NULL allocation attempt for zero-byte requests, so
 * zero sizes are rounded up to one byte before reaching malloc/calloc/realloc.
 */
static inline void *PyMem_Malloc(size_t size) {
    return malloc(size == 0 ? 1 : size);
}

static inline void *PyMem_Calloc(size_t nelem, size_t elsize) {
    if (nelem == 0 || elsize == 0) {
        return calloc(1, 1);
    }
    return calloc(nelem, elsize);
}

static inline void *PyMem_Realloc(void *ptr, size_t new_size) {
    return realloc(ptr, new_size == 0 ? 1 : new_size);
}

static inline void PyMem_Free(void *ptr) {
    free(ptr);
}

static inline void *PyMem_RawMalloc(size_t size) {
    return malloc(size == 0 ? 1 : size);
}

static inline void *PyMem_RawCalloc(size_t nelem, size_t elsize) {
    if (nelem == 0 || elsize == 0) {
        return calloc(1, 1);
    }
    return calloc(nelem, elsize);
}

static inline void *PyMem_RawRealloc(void *ptr, size_t new_size) {
    return realloc(ptr, new_size == 0 ? 1 : new_size);
}

static inline void PyMem_RawFree(void *ptr) {
    free(ptr);
}


/* Pon has no GIL. These calls preserve CPython source structure while doing no
 * synchronization; PyEval_SaveThread returns a non-NULL sentinel supplied by
 * the runtime table so bracket code can honestly test it.
 */
static inline PyGILState_STATE PyGILState_Ensure(void) {
    return PyGILState_LOCKED;
}

static inline void PyGILState_Release(PyGILState_STATE state) {
    (void)state;
}

static inline PyThreadState *PyEval_SaveThread(void) {
    return PyPon_Capi()->runtime_->eval_save_thread();
}

static inline void PyEval_RestoreThread(PyThreadState *state) {
    PyPon_Capi()->runtime_->eval_restore_thread(state);
}

#define Py_BEGIN_ALLOW_THREADS { PyThreadState *_save; _save = PyEval_SaveThread();
#define Py_BLOCK_THREADS PyEval_RestoreThread(_save);
#define Py_UNBLOCK_THREADS _save = PyEval_SaveThread();
#define Py_END_ALLOW_THREADS PyEval_RestoreThread(_save); }

static inline PyObject *PyCapsule_New(void *pointer, const char *name, PyCapsule_Destructor destructor) {
    return PyPon_Capi()->runtime_->capsule_new(pointer, name, destructor);
}

static inline void *PyCapsule_GetPointer(PyObject *capsule, const char *name) {
    return PyPon_Capi()->runtime_->capsule_get_pointer(capsule, name);
}

static inline int PyCapsule_IsValid(PyObject *capsule, const char *name) {
    return PyPon_Capi()->runtime_->capsule_is_valid(capsule, name);
}

static inline int PyCapsule_SetContext(PyObject *capsule, void *context) {
    return PyPon_Capi()->runtime_->capsule_set_context(capsule, context);
}

static inline void *PyCapsule_GetContext(PyObject *capsule) {
    return PyPon_Capi()->runtime_->capsule_get_context(capsule);
}

static inline void *PyCapsule_Import(const char *name, int no_block) {
    return PyPon_Capi()->runtime_->capsule_import(name, no_block);
}

static inline PyObject *PyImport_ImportModule(const char *name) {
    return PyPon_Capi()->runtime_->import_import_module(name);
}

static inline PyObject *PyImport_AddModule(const char *name) {
    return PyPon_Capi()->runtime_->import_add_module(name);
}

static inline PyObject *PyModule_GetDict(PyObject *module) {
    return PyPon_Capi()->runtime_->module_get_dict(module);
}

static inline void *PyModule_GetState(PyObject *module) {
    return PyPon_Capi()->runtime_->module_get_state(module);
}

static inline const char *PyModule_GetName(PyObject *module) {
    return PyPon_Capi()->runtime_->module_get_name(module);
}

static inline PyObject *PySys_GetObject(const char *name) {
    return PyPon_Capi()->runtime_->sys_get_object(name);
}

#endif /* PON_CAPI_RUNTIME_INLINE_H */
