#ifndef PON_CAPI_NUMBERS_H
#define PON_CAPI_NUMBERS_H

/* Numbers family: int/bool/float/complex construction and extraction. */

typedef struct PyPonCapiNumbers {
    PyObject *(*long_from_long)(long);
    long (*long_as_long)(PyObject *);
    PyObject *(*long_from_long_long)(long long);
    PyObject *(*long_from_unsigned_long)(unsigned long);
    PyObject *(*long_from_unsigned_long_long)(unsigned long long);
    PyObject *(*long_from_ssize_t)(Py_ssize_t);
    PyObject *(*long_from_size_t)(size_t);
    PyObject *(*long_from_double)(double);
    long long (*long_as_long_long)(PyObject *);
    unsigned long (*long_as_unsigned_long)(PyObject *);
    unsigned long (*long_as_unsigned_long_mask)(PyObject *);
    Py_ssize_t (*long_as_ssize_t)(PyObject *);
    size_t (*long_as_size_t)(PyObject *);
    double (*long_as_double)(PyObject *);
    long (*long_as_long_and_overflow)(PyObject *, int *);
    PyObject *(*long_from_void_ptr)(void *);
    void *(*long_as_void_ptr)(PyObject *);
    PyObject *(*bool_from_long)(long);
    PyObject *(*float_from_double)(double);
    double (*float_as_double)(PyObject *);
    PyObject *(*complex_from_doubles)(double, double);
    double (*complex_real_as_double)(PyObject *);
    double (*complex_imag_as_double)(PyObject *);
    int (*index_check)(PyObject *);
    PyObject *(*number_index)(PyObject *);
    PyObject *(*number_long)(PyObject *);
    PyObject *(*number_float)(PyObject *);
    Py_ssize_t (*number_as_ssize_t)(PyObject *, PyObject *);
    int (*type_check)(PyObject *, int);
    unsigned long long (*long_as_unsigned_long_long)(PyObject *);
    /* Family expansion point: append fields only; never reorder. */
} PyPonCapiNumbers;

#endif /* PON_CAPI_NUMBERS_H */
