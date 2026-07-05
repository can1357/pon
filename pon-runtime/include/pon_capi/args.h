#ifndef PON_CAPI_ARGS_H
#define PON_CAPI_ARGS_H

/* Argument parsing and value construction live in pon_capi_args.c.  They are
 * ordinary extension-local C symbols, not a family table: the implementation
 * dispatches through the injected core/numbers/strings/containers/object
 * tables exposed by Python.h. */

typedef int (*PyPonArgConverter)(PyObject *, void *);

int PyArg_ParseTuple(PyObject *args, const char *format, ...);
int PyArg_VaParse(PyObject *args, const char *format, va_list vargs);
int PyArg_ParseTupleAndKeywords(PyObject *args, PyObject *kwargs, const char *format, char **kwlist, ...);
int PyArg_VaParseTupleAndKeywords(PyObject *args, PyObject *kwargs, const char *format, char **kwlist, va_list vargs);
int PyArg_UnpackTuple(PyObject *args, const char *name, Py_ssize_t min, Py_ssize_t max, ...);

PyObject *Py_BuildValue(const char *format, ...);
PyObject *Py_VaBuildValue(const char *format, va_list vargs);

void PyBuffer_Release(Py_buffer *view);

#endif /* PON_CAPI_ARGS_H */
