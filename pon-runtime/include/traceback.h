#ifndef PON_TRACEBACK_H
#define PON_TRACEBACK_H

#ifndef PON_PYTHON_H
#include "Python.h"
#endif

#ifndef PON_FRAMEOBJECT_H
#include "frameobject.h"
#endif

static inline int PyTraceBack_Here(PyFrameObject *frame) {
    return PyPon_Capi()->runtime_->traceback_here(frame);
}

#ifndef PON_HAVE_PYTRACEBACK_INLINE
#define PON_HAVE_PYTRACEBACK_INLINE 1
static inline int PyTraceBack_Check(PyObject *object) {
    return PyPon_Capi()->runtime_->traceback_check(object);
}
#endif

#endif /* PON_TRACEBACK_H */
