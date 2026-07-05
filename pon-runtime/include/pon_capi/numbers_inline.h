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

static inline unsigned long long PyLong_AsUnsignedLongLongMask(PyObject *object) {
    return PyPon_Capi()->numbers->long_as_unsigned_long_long_mask(object);
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

static inline long long PyLong_AsLongLongAndOverflow(PyObject *object, int *overflow) {
    return PyPon_Capi()->numbers->long_as_long_long_and_overflow(object, overflow);
}

static inline int PyLong_IsZero(PyObject *object) {
    return PyPon_Capi()->numbers->long_is_zero(object);
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

#define PyFloat_AS_DOUBLE(object) PyFloat_AsDouble((PyObject *)(object))

static inline PyObject *PyFloat_FromString(PyObject *object) {
    return PyPon_Capi()->numbers->float_from_string(object);
}

static inline double PyOS_string_to_double(const char *text, char **endptr, PyObject *overflow_exception) {
    return PyPon_Capi()->numbers->os_string_to_double(text, endptr, overflow_exception);
}

static inline PyObject *PyComplex_FromDoubles(double real, double imag) {
    return PyPon_Capi()->numbers->complex_from_doubles(real, imag);
}

static inline PyObject *PyComplex_FromCComplex(Py_complex value) {
    return PyPon_Capi()->numbers->complex_from_c_complex(value);
}

static inline Py_complex PyComplex_AsCComplex(PyObject *object) {
    return PyPon_Capi()->numbers->complex_as_c_complex(object);
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

static inline PyObject *PyNumber_Add(PyObject *left, PyObject *right) {
    return PyPon_Capi()->numbers->number_add(left, right);
}

static inline PyObject *PyNumber_Subtract(PyObject *left, PyObject *right) {
    return PyPon_Capi()->numbers->number_subtract(left, right);
}

static inline PyObject *PyNumber_Multiply(PyObject *left, PyObject *right) {
    return PyPon_Capi()->numbers->number_multiply(left, right);
}

static inline PyObject *PyNumber_TrueDivide(PyObject *left, PyObject *right) {
    return PyPon_Capi()->numbers->number_true_divide(left, right);
}

static inline PyObject *PyNumber_FloorDivide(PyObject *left, PyObject *right) {
    return PyPon_Capi()->numbers->number_floor_divide(left, right);
}

static inline PyObject *PyNumber_Remainder(PyObject *left, PyObject *right) {
    return PyPon_Capi()->numbers->number_remainder(left, right);
}

static inline PyObject *PyNumber_Divmod(PyObject *left, PyObject *right) {
    return PyPon_Capi()->numbers->number_divmod(left, right);
}

static inline PyObject *PyNumber_Power(PyObject *left, PyObject *right, PyObject *modulo) {
    return PyPon_Capi()->numbers->number_power(left, right, modulo);
}

static inline PyObject *PyNumber_Negative(PyObject *object) {
    return PyPon_Capi()->numbers->number_negative(object);
}

static inline PyObject *PyNumber_Positive(PyObject *object) {
    return PyPon_Capi()->numbers->number_positive(object);
}

static inline PyObject *PyNumber_Absolute(PyObject *object) {
    return PyPon_Capi()->numbers->number_absolute(object);
}

static inline PyObject *PyNumber_Invert(PyObject *object) {
    return PyPon_Capi()->numbers->number_invert(object);
}

static inline PyObject *PyNumber_Lshift(PyObject *left, PyObject *right) {
    return PyPon_Capi()->numbers->number_lshift(left, right);
}

static inline PyObject *PyNumber_Rshift(PyObject *left, PyObject *right) {
    return PyPon_Capi()->numbers->number_rshift(left, right);
}

static inline PyObject *PyNumber_And(PyObject *left, PyObject *right) {
    return PyPon_Capi()->numbers->number_and(left, right);
}

static inline PyObject *PyNumber_Xor(PyObject *left, PyObject *right) {
    return PyPon_Capi()->numbers->number_xor(left, right);
}

static inline PyObject *PyNumber_Or(PyObject *left, PyObject *right) {
    return PyPon_Capi()->numbers->number_or(left, right);
}

static inline PyObject *PyNumber_MatrixMultiply(PyObject *left, PyObject *right) {
    return PyPon_Capi()->numbers->number_matrix_multiply(left, right);
}

static inline PyObject *PyNumber_InPlaceAdd(PyObject *left, PyObject *right) {
    return PyPon_Capi()->numbers->number_inplace_add(left, right);
}

static inline PyObject *PyNumber_InPlaceSubtract(PyObject *left, PyObject *right) {
    return PyPon_Capi()->numbers->number_inplace_subtract(left, right);
}

static inline PyObject *PyNumber_InPlaceMultiply(PyObject *left, PyObject *right) {
    return PyPon_Capi()->numbers->number_inplace_multiply(left, right);
}

static inline PyObject *PyNumber_InPlaceTrueDivide(PyObject *left, PyObject *right) {
    return PyPon_Capi()->numbers->number_inplace_true_divide(left, right);
}

static inline PyObject *PyNumber_InPlaceFloorDivide(PyObject *left, PyObject *right) {
    return PyPon_Capi()->numbers->number_inplace_floor_divide(left, right);
}

static inline PyObject *PyNumber_InPlaceRemainder(PyObject *left, PyObject *right) {
    return PyPon_Capi()->numbers->number_inplace_remainder(left, right);
}

static inline PyObject *PyNumber_InPlacePower(PyObject *left, PyObject *right, PyObject *modulo) {
    return PyPon_Capi()->numbers->number_inplace_power(left, right, modulo);
}

static inline PyObject *PyNumber_InPlaceLshift(PyObject *left, PyObject *right) {
    return PyPon_Capi()->numbers->number_inplace_lshift(left, right);
}

static inline PyObject *PyNumber_InPlaceRshift(PyObject *left, PyObject *right) {
    return PyPon_Capi()->numbers->number_inplace_rshift(left, right);
}

static inline PyObject *PyNumber_InPlaceAnd(PyObject *left, PyObject *right) {
    return PyPon_Capi()->numbers->number_inplace_and(left, right);
}

static inline PyObject *PyNumber_InPlaceXor(PyObject *left, PyObject *right) {
    return PyPon_Capi()->numbers->number_inplace_xor(left, right);
}

static inline PyObject *PyNumber_InPlaceOr(PyObject *left, PyObject *right) {
    return PyPon_Capi()->numbers->number_inplace_or(left, right);
}

static inline PyObject *PyNumber_InPlaceMatrixMultiply(PyObject *left, PyObject *right) {
    return PyPon_Capi()->numbers->number_inplace_matrix_multiply(left, right);
}

#endif /* PON_CAPI_NUMBERS_INLINE_H */
