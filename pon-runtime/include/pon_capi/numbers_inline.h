#ifndef PON_CAPI_NUMBERS_INLINE_H
#define PON_CAPI_NUMBERS_INLINE_H

/* Inline wrappers for the numbers family. Included by Python.h after the
 * PyPonCapi definition; never include directly.
 */

static inline int PyLong_Check(PyObject *object) {
    return PyPon_Capi()->numbers->type_check(object, PON_TID_LONG);
}

static inline int PyLong_CheckExact(PyObject *object) {
    return PyPon_Capi()->core->builtin_type_id(object) == PON_TID_LONG;
}

static inline int PyBool_Check(PyObject *object) {
    return PyPon_Capi()->numbers->type_check(object, PON_TID_BOOL);
}

static inline int PyFloat_Check(PyObject *object) {
    return PyPon_Capi()->numbers->type_check(object, PON_TID_FLOAT);
}

static inline int PyFloat_CheckExact(PyObject *object) {
    return PyPon_Capi()->core->builtin_type_id(object) == PON_TID_FLOAT;
}

static inline int PyComplex_Check(PyObject *object) {
    return PyPon_Capi()->numbers->type_check(object, PON_TID_COMPLEX);
}

static inline int PyComplex_CheckExact(PyObject *object) {
    return PyPon_Capi()->core->builtin_type_id(object) == PON_TID_COMPLEX;
}

static inline PyObject *PyLong_FromLong(long value) {
    return PyPon_Capi()->numbers->long_from_long(value);
}

static inline PyObject *PyLong_FromLongLong(long long value) {
    return PyPon_Capi()->numbers->long_from_long_long(value);
}

static inline PyObject *PyLong_FromUnsignedLong(unsigned long value) {
    return PyPon_Capi()->numbers->long_from_unsigned_long(value);
}

static inline PyObject *PyLong_FromUnsignedLongLong(unsigned long long value) {
    return PyPon_Capi()->numbers->long_from_unsigned_long_long(value);
}

static inline PyObject *PyLong_FromSsize_t(Py_ssize_t value) {
    return PyPon_Capi()->numbers->long_from_ssize_t(value);
}

static inline PyObject *PyLong_FromSize_t(size_t value) {
    return PyPon_Capi()->numbers->long_from_size_t(value);
}

static inline PyObject *PyLong_FromDouble(double value) {
    return PyPon_Capi()->numbers->long_from_double(value);
}

static inline long PyLong_AsLong(PyObject *object) {
    return PyPon_Capi()->numbers->long_as_long(object);
}

static inline long long PyLong_AsLongLong(PyObject *object) {
    return PyPon_Capi()->numbers->long_as_long_long(object);
}

static inline unsigned long PyLong_AsUnsignedLong(PyObject *object) {
    return PyPon_Capi()->numbers->long_as_unsigned_long(object);
}

static inline unsigned long long PyLong_AsUnsignedLongLong(PyObject *object) {
    return PyPon_Capi()->numbers->long_as_unsigned_long_long(object);
}

static inline unsigned long PyLong_AsUnsignedLongMask(PyObject *object) {
    return PyPon_Capi()->numbers->long_as_unsigned_long_mask(object);
}

static inline Py_ssize_t PyLong_AsSsize_t(PyObject *object) {
    return PyPon_Capi()->numbers->long_as_ssize_t(object);
}

static inline size_t PyLong_AsSize_t(PyObject *object) {
    return PyPon_Capi()->numbers->long_as_size_t(object);
}

static inline double PyLong_AsDouble(PyObject *object) {
    return PyPon_Capi()->numbers->long_as_double(object);
}

static inline long PyLong_AsLongAndOverflow(PyObject *object, int *overflow) {
    return PyPon_Capi()->numbers->long_as_long_and_overflow(object, overflow);
}

static inline PyObject *PyLong_FromVoidPtr(void *value) {
    return PyPon_Capi()->numbers->long_from_void_ptr(value);
}

static inline void *PyLong_AsVoidPtr(PyObject *object) {
    return PyPon_Capi()->numbers->long_as_void_ptr(object);
}

static inline PyObject *PyBool_FromLong(long value) {
    return PyPon_Capi()->numbers->bool_from_long(value);
}

static inline PyObject *PyFloat_FromDouble(double value) {
    return PyPon_Capi()->numbers->float_from_double(value);
}

static inline double PyFloat_AsDouble(PyObject *object) {
    return PyPon_Capi()->numbers->float_as_double(object);
}

static inline PyObject *PyComplex_FromDoubles(double real, double imag) {
    return PyPon_Capi()->numbers->complex_from_doubles(real, imag);
}

static inline double PyComplex_RealAsDouble(PyObject *object) {
    return PyPon_Capi()->numbers->complex_real_as_double(object);
}

static inline double PyComplex_ImagAsDouble(PyObject *object) {
    return PyPon_Capi()->numbers->complex_imag_as_double(object);
}

static inline int PyIndex_Check(PyObject *object) {
    return PyPon_Capi()->numbers->index_check(object);
}

static inline PyObject *PyNumber_Index(PyObject *object) {
    return PyPon_Capi()->numbers->number_index(object);
}

static inline PyObject *PyNumber_Long(PyObject *object) {
    return PyPon_Capi()->numbers->number_long(object);
}

static inline PyObject *PyNumber_Float(PyObject *object) {
    return PyPon_Capi()->numbers->number_float(object);
}

static inline Py_ssize_t PyNumber_AsSsize_t(PyObject *object, PyObject *exc) {
    return PyPon_Capi()->numbers->number_as_ssize_t(object, exc);
}

#endif /* PON_CAPI_NUMBERS_INLINE_H */
