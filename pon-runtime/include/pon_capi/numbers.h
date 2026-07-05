#ifndef PON_CAPI_NUMBERS_H
#define PON_CAPI_NUMBERS_H

/* Numbers family: int/bool/float/complex construction and extraction. */

typedef PyObject PyLongObject;

#define Py_ASNATIVEBYTES_DEFAULTS -1
#define Py_ASNATIVEBYTES_BIG_ENDIAN 0
#define Py_ASNATIVEBYTES_LITTLE_ENDIAN 1
#define Py_ASNATIVEBYTES_NATIVE_ENDIAN 3
#define Py_ASNATIVEBYTES_UNSIGNED_BUFFER 4
#define Py_ASNATIVEBYTES_REJECT_NEGATIVE 8
#define Py_ASNATIVEBYTES_ALLOW_INDEX 16

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
    int (*long_is_zero)(PyObject *);
    unsigned long long (*long_as_unsigned_long_long_mask)(PyObject *);
    long long (*long_as_long_long_and_overflow)(PyObject *, int *);
    PyObject *(*float_from_string)(PyObject *);
    double (*os_string_to_double)(const char *, char **, PyObject *);
    PyObject *(*complex_from_c_complex)(Py_complex);
    Py_complex (*complex_as_c_complex)(PyObject *);
    PyObject *(*number_add)(PyObject *, PyObject *);
    PyObject *(*number_subtract)(PyObject *, PyObject *);
    PyObject *(*number_multiply)(PyObject *, PyObject *);
    PyObject *(*number_true_divide)(PyObject *, PyObject *);
    PyObject *(*number_floor_divide)(PyObject *, PyObject *);
    PyObject *(*number_remainder)(PyObject *, PyObject *);
    PyObject *(*number_divmod)(PyObject *, PyObject *);
    PyObject *(*number_power)(PyObject *, PyObject *, PyObject *);
    PyObject *(*number_negative)(PyObject *);
    PyObject *(*number_positive)(PyObject *);
    PyObject *(*number_absolute)(PyObject *);
    PyObject *(*number_invert)(PyObject *);
    PyObject *(*number_lshift)(PyObject *, PyObject *);
    PyObject *(*number_rshift)(PyObject *, PyObject *);
    PyObject *(*number_and)(PyObject *, PyObject *);
    PyObject *(*number_xor)(PyObject *, PyObject *);
    PyObject *(*number_or)(PyObject *, PyObject *);
    PyObject *(*number_matrix_multiply)(PyObject *, PyObject *);
    PyObject *(*number_inplace_add)(PyObject *, PyObject *);
    PyObject *(*number_inplace_subtract)(PyObject *, PyObject *);
    PyObject *(*number_inplace_multiply)(PyObject *, PyObject *);
    PyObject *(*number_inplace_true_divide)(PyObject *, PyObject *);
    PyObject *(*number_inplace_floor_divide)(PyObject *, PyObject *);
    PyObject *(*number_inplace_remainder)(PyObject *, PyObject *);
    PyObject *(*number_inplace_power)(PyObject *, PyObject *, PyObject *);
    PyObject *(*number_inplace_lshift)(PyObject *, PyObject *);
    PyObject *(*number_inplace_rshift)(PyObject *, PyObject *);
    PyObject *(*number_inplace_and)(PyObject *, PyObject *);
    PyObject *(*number_inplace_xor)(PyObject *, PyObject *);
    PyObject *(*number_inplace_or)(PyObject *, PyObject *);
    PyObject *(*number_inplace_matrix_multiply)(PyObject *, PyObject *);
    int (*number_check)(PyObject *);
    Py_hash_t (*hash_double)(PyObject *, double);
    Py_ssize_t (*long_as_native_bytes)(PyObject *, void *, Py_ssize_t, int);
    PyObject *(*long_from_native_bytes)(const void *, size_t, int);
    PyObject *(*long_from_unsigned_native_bytes)(const void *, size_t, int);
    PyObject *(*long_from_string)(const char *, char **, int);
    /* Family expansion point: append fields only; never reorder. */
} PyPonCapiNumbers;

#endif /* PON_CAPI_NUMBERS_H */
