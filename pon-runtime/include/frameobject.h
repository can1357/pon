#ifndef PON_FRAMEOBJECT_H
#define PON_FRAMEOBJECT_H

#ifndef PON_PYTHON_H
#include "Python.h"
#endif

/* Cython's traceback path writes `frame->f_lineno` directly.  Keep this
 * concrete C face layout in lock-step with Rust's `abi::PyFrame` prefix:
 * PyObjectHeader, state, n_locals, locals, parent, exc_state, line.  The
 * `f_lineno` write therefore lands on the runtime frame line slot that
 * PyTraceBack_Here reads when it prepends a traceback entry.
 */
struct _frame {
    PyObject_HEAD
    uint32_t f_state;
    uint32_t f_nlocals;
    PyObject **f_localsplus;
    union {
        PyObject *f_parent;
        PyFrameObject *f_back;
    };
    PyObject *f_exc_state;
    int f_lineno;
};

#define _PON_ALIGN_UP_SIZE(n, a) ((((n) + (a) - 1) / (a)) * (a))
#define _PON_PYFRAME_LINE_OFFSET \
    (sizeof(PyObject) + sizeof(uint32_t) + sizeof(uint32_t) + sizeof(PyObject **) + sizeof(PyObject *) + sizeof(PyObject *))
_Static_assert(sizeof(((struct _frame *)0)->f_lineno) == sizeof(uint32_t),
               "Pon PyFrameObject f_lineno must match abi::PyFrame::line width");
_Static_assert(offsetof(struct _frame, f_lineno) == _PON_PYFRAME_LINE_OFFSET,
               "Pon PyFrameObject f_lineno must sit at abi::PyFrame::line offset");
_Static_assert(sizeof(struct _frame) == _PON_ALIGN_UP_SIZE(_PON_PYFRAME_LINE_OFFSET + sizeof(uint32_t), sizeof(void *)),
               "Pon PyFrameObject C face must mirror abi::PyFrame prefix size");

#ifndef PON_CODEOBJECT_STRUCT_DEFINED
#define PON_CODEOBJECT_STRUCT_DEFINED 1
/* Minimal code-object face for Cython's generated code.  Pon does not expose
 * CPython bytecode; these fields are metadata captured by PyCode_New. */
struct PyCodeObject {
    PyObject_HEAD
    int _co_firsttraceable;
    int co_firstlineno;
    PyObject *co_filename;
    PyObject *co_name;
    PyObject *co_qualname;
    int co_nfreevars;
};
#endif

static inline PyFrameObject *PyFrame_New(PyThreadState *tstate, PyCodeObject *code, PyObject *globals, PyObject *locals) {
    return PyPon_Capi()->runtime_->frame_new(tstate, code, globals, locals);
}

static inline int PyFrame_SetLineNumber(PyFrameObject *frame, int lineno) {
    if (frame == NULL) {
        return -1;
    }
    frame->f_lineno = lineno;
    return 0;
}

static inline int _PyFrame_SetLineNumber(PyFrameObject *frame, int lineno) {
    return PyFrame_SetLineNumber(frame, lineno);
}

static inline PyCodeObject *PyCode_NewEmpty(const char *filename, const char *funcname, int firstlineno) {
    return PyPon_Capi()->runtime_->code_new_empty(filename, funcname, firstlineno);
}

static inline PyCodeObject *PyCode_New(
    int argcount,
    int kwonlyargcount,
    int nlocals,
    int stacksize,
    int flags,
    PyObject *code,
    PyObject *consts,
    PyObject *names,
    PyObject *varnames,
    PyObject *freevars,
    PyObject *cellvars,
    PyObject *filename,
    PyObject *name,
    PyObject *qualname,
    int firstlineno,
    PyObject *linetable,
    PyObject *exceptiontable)
{
    return PyPon_Capi()->runtime_->code_new(
        argcount, kwonlyargcount, nlocals, stacksize, flags, code, consts, names,
        varnames, freevars, cellvars, filename, name, qualname, firstlineno,
        linetable, exceptiontable);
}

static inline PyCodeObject *PyCode_NewWithPosOnlyArgs(
    int argcount,
    int posonlyargcount,
    int kwonlyargcount,
    int nlocals,
    int stacksize,
    int flags,
    PyObject *code,
    PyObject *consts,
    PyObject *names,
    PyObject *varnames,
    PyObject *freevars,
    PyObject *cellvars,
    PyObject *filename,
    PyObject *name,
    PyObject *qualname,
    int firstlineno,
    PyObject *linetable,
    PyObject *exceptiontable)
{
    return PyPon_Capi()->runtime_->code_new_with_posonly_args(
        argcount, posonlyargcount, kwonlyargcount, nlocals, stacksize, flags,
        code, consts, names, varnames, freevars, cellvars, filename, name,
        qualname, firstlineno, linetable, exceptiontable);
}

#define PyUnstable_Code_NewWithPosOnlyArgs PyCode_NewWithPosOnlyArgs

static inline int PyCode_GetNumFree(PyCodeObject *code) {
    return PyPon_Capi()->runtime_->code_get_num_free(code);
}

static inline int PyCode_HasFreeVars(PyCodeObject *code) {
    return PyPon_Capi()->runtime_->code_has_free_vars(code);
}

#endif /* PON_FRAMEOBJECT_H */
