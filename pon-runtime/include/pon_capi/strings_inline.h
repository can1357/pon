#ifndef PON_CAPI_STRINGS_INLINE_H
#define PON_CAPI_STRINGS_INLINE_H

/* Inline wrapper layer for the strings family. Included by Python.h AFTER the
 * PyPonCapi definition and core_inline.h; never include directly.
 */

#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* ---- unicode ---- */

static inline PyObject *PyUnicode_FromString(const char *value) {
    return PyPon_Capi()->strings->unicode_from_string(value);
}

static inline PyObject *PyUnicode_FromStringAndSize(const char *value, Py_ssize_t size) {
    return PyPon_Capi()->strings->unicode_from_string_and_size(value, size);
}

static inline const char *PyUnicode_AsUTF8(PyObject *object) {
    return PyPon_Capi()->strings->unicode_as_utf8(object);
}

static inline const char *PyUnicode_AsUTF8AndSize(PyObject *object, Py_ssize_t *size) {
    return PyPon_Capi()->strings->unicode_as_utf8_and_size(object, size);
}

static inline Py_ssize_t PyUnicode_GetLength(PyObject *object) {
    return PyPon_Capi()->strings->unicode_get_length(object);
}

static inline PyObject *PyUnicode_DecodeUTF8(const char *value, Py_ssize_t size, const char *errors) {
    return PyPon_Capi()->strings->unicode_decode_utf8(value, size, errors);
}

static inline PyObject *PyUnicode_DecodeASCII(const char *value, Py_ssize_t size, const char *errors) {
    return PyPon_Capi()->strings->unicode_decode_ascii(value, size, errors);
}

static inline PyObject *PyUnicode_DecodeLatin1(const char *value, Py_ssize_t size, const char *errors) {
    return PyPon_Capi()->strings->unicode_decode_latin1(value, size, errors);
}

static inline PyObject *PyUnicode_AsUTF8String(PyObject *object) {
    return PyPon_Capi()->strings->unicode_as_utf8_string(object);
}

static inline PyObject *PyUnicode_AsASCIIString(PyObject *object) {
    return PyPon_Capi()->strings->unicode_as_ascii_string(object);
}

static inline PyObject *PyUnicode_InternFromString(const char *value) {
    return PyPon_Capi()->strings->unicode_intern_from_string(value);
}

static inline int PyUnicode_Compare(PyObject *left, PyObject *right) {
    return PyPon_Capi()->strings->unicode_compare(left, right);
}

static inline int PyUnicode_CompareWithASCIIString(PyObject *left, const char *right) {
    return PyPon_Capi()->strings->unicode_compare_with_ascii_string(left, right);
}

static inline PyObject *PyUnicode_Concat(PyObject *left, PyObject *right) {
    return PyPon_Capi()->strings->unicode_concat(left, right);
}

static inline int PyUnicode_Check(PyObject *object) {
    return PyPon_Capi()->strings->unicode_check(object);
}

static inline int PyUnicode_CheckExact(PyObject *object) {
    return PyPon_Capi()->strings->unicode_check_exact(object);
}

/* ---- bytes ---- */

static inline PyObject *PyBytes_FromStringAndSize(const char *value, Py_ssize_t size) {
    return PyPon_Capi()->strings->bytes_from_string_and_size(value, size);
}

static inline PyObject *PyBytes_FromString(const char *value) {
    return PyPon_Capi()->strings->bytes_from_string(value);
}

static inline Py_ssize_t PyBytes_Size(PyObject *object) {
    return PyPon_Capi()->strings->bytes_size(object);
}

static inline char *PyBytes_AsString(PyObject *object) {
    return PyPon_Capi()->strings->bytes_as_string(object);
}

static inline int PyBytes_AsStringAndSize(PyObject *object, char **buffer, Py_ssize_t *size) {
    return PyPon_Capi()->strings->bytes_as_string_and_size(object, buffer, size);
}

static inline void PyBytes_Concat(PyObject **bytes, PyObject *newpart) {
    PyPon_Capi()->strings->bytes_concat(bytes, newpart);
}

static inline int PyBytes_Check(PyObject *object) {
    return PyPon_Capi()->strings->bytes_check(object);
}

static inline int PyBytes_CheckExact(PyObject *object) {
    return PyPon_Capi()->strings->bytes_check_exact(object);
}

/* ---- bytearray ---- */

static inline PyObject *PyByteArray_FromStringAndSize(const char *value, Py_ssize_t size) {
    return PyPon_Capi()->strings->bytearray_from_string_and_size(value, size);
}

static inline Py_ssize_t PyByteArray_Size(PyObject *object) {
    return PyPon_Capi()->strings->bytearray_size(object);
}

static inline char *PyByteArray_AsString(PyObject *object) {
    return PyPon_Capi()->strings->bytearray_as_string(object);
}

static inline int PyByteArray_Check(PyObject *object) {
    return PyPon_Capi()->strings->bytearray_check(object);
}

static inline int PyByteArray_CheckExact(PyObject *object) {
    return PyPon_Capi()->strings->bytearray_check_exact(object);
}

/* ---- PyUnicode_FromFormat / PyUnicode_FromFormatV ---- */

static inline int _PyPon_FormatAppend(char *out, Py_ssize_t capacity, Py_ssize_t *used, const char *text, Py_ssize_t len) {
    if (text == NULL) {
        text = "(null)";
        len = 6;
    }
    if (len < 0) {
        len = (Py_ssize_t)strlen(text);
    }
    if (len > PY_SSIZE_T_MAX - *used) {
        PyErr_SetString(PyExc_ValueError, "PyUnicode_FromFormat result is too large");
        return -1;
    }
    if (out != NULL) {
        if (capacity <= 0 || len > capacity - 1 - *used) {
            PyErr_SetString(PyExc_ValueError, "PyUnicode_FromFormat render buffer is too small");
            return -1;
        }
        memcpy(out + *used, text, (size_t)len);
    }
    *used += len;
    return 0;
}

static inline int _PyPon_FormatAppendNumber(char *out, Py_ssize_t capacity, Py_ssize_t *used, const char *printf_format, long long value) {
    char stack[64];
    int written = snprintf(stack, sizeof(stack), printf_format, value);
    if (written < 0 || (size_t)written >= sizeof(stack)) {
        PyErr_SetString(PyExc_ValueError, "PyUnicode_FromFormat numeric conversion failed");
        return -1;
    }
    return _PyPon_FormatAppend(out, capacity, used, stack, (Py_ssize_t)written);
}

static inline int _PyPon_FormatAppendUnsigned(char *out, Py_ssize_t capacity, Py_ssize_t *used, const char *printf_format, unsigned long long value) {
    char stack[64];
    int written = snprintf(stack, sizeof(stack), printf_format, value);
    if (written < 0 || (size_t)written >= sizeof(stack)) {
        PyErr_SetString(PyExc_ValueError, "PyUnicode_FromFormat unsigned conversion failed");
        return -1;
    }
    return _PyPon_FormatAppend(out, capacity, used, stack, (Py_ssize_t)written);
}

static inline int _PyPon_FormatAppendObject(char *out, Py_ssize_t capacity, Py_ssize_t *used, PyObject *object, int repr) {
    PyObject *rendered = repr ? PyPon_Capi()->strings->object_repr(object) : PyPon_Capi()->strings->object_str(object);
    if (rendered == NULL) {
        return -1;
    }
    Py_ssize_t size = 0;
    const char *text = PyUnicode_AsUTF8AndSize(rendered, &size);
    if (text == NULL) {
        return -1;
    }
    return _PyPon_FormatAppend(out, capacity, used, text, size);
}

static inline Py_ssize_t _PyPon_FormatUnicodeInto(char *out, Py_ssize_t capacity, const char *format, va_list vargs) {
    Py_ssize_t used = 0;
    const char *cursor = format;
    while (*cursor != '\0') {
        if (*cursor != '%') {
            if (_PyPon_FormatAppend(out, capacity, &used, cursor, 1) < 0) {
                return -1;
            }
            cursor++;
            continue;
        }

        cursor++;
        if (*cursor == '\0') {
            PyErr_SetString(PyExc_ValueError, "incomplete PyUnicode_FromFormat format");
            return -1;
        }

        if (*cursor == '%') {
            if (_PyPon_FormatAppend(out, capacity, &used, "%", 1) < 0) {
                return -1;
            }
        } else if (*cursor == 's') {
            const char *text = va_arg(vargs, const char *);
            if (_PyPon_FormatAppend(out, capacity, &used, text, -1) < 0) {
                return -1;
            }
        } else if (*cursor == 'p') {
            void *value = va_arg(vargs, void *);
            char stack[32];
            int written = snprintf(stack, sizeof(stack), "%p", value);
            if (written < 0 || (size_t)written >= sizeof(stack)) {
                PyErr_SetString(PyExc_ValueError, "PyUnicode_FromFormat pointer conversion failed");
                return -1;
            }
            if (_PyPon_FormatAppend(out, capacity, &used, stack, (Py_ssize_t)written) < 0) {
                return -1;
            }
        } else if (*cursor == 'U') {
            PyObject *object = va_arg(vargs, PyObject *);
            if (_PyPon_FormatAppendObject(out, capacity, &used, object, 0) < 0) {
                return -1;
            }
        } else if (*cursor == 'd' || *cursor == 'i' || *cursor == 'u' || *cursor == 'x' || *cursor == 'X'
                   || *cursor == 'l' || *cursor == 'z' || *cursor == 't' || *cursor == 'j') {
            /* CPython length modifiers: l, ll, z, t, j on d/i/u/x/X. Each
             * modifier extracts its EXACT C type (va_arg with a merely
             * same-width type is undefined behavior). */
            char modifier = 0; /* 0, 'l', 'L' (= ll), 'z', 't', 'j' */
            if (*cursor == 'l') {
                modifier = 'l';
                cursor++;
                if (*cursor == 'l') {
                    modifier = 'L';
                    cursor++;
                }
            } else if (*cursor == 'z' || *cursor == 't' || *cursor == 'j') {
                modifier = *cursor;
                cursor++;
            }
            if (*cursor == 'd' || *cursor == 'i') {
                long long value;
                switch (modifier) {
                    case 'l': value = va_arg(vargs, long); break;
                    case 'L': value = va_arg(vargs, long long); break;
                    case 'z': value = (long long)va_arg(vargs, Py_ssize_t); break;
                    case 't': value = (long long)va_arg(vargs, ptrdiff_t); break;
                    case 'j': value = (long long)va_arg(vargs, intmax_t); break;
                    default: value = va_arg(vargs, int); break;
                }
                if (_PyPon_FormatAppendNumber(out, capacity, &used, "%lld", value) < 0) {
                    return -1;
                }
            } else if (*cursor == 'u' || *cursor == 'x' || *cursor == 'X') {
                unsigned long long value;
                switch (modifier) {
                    case 'l': value = va_arg(vargs, unsigned long); break;
                    case 'L': value = va_arg(vargs, unsigned long long); break;
                    case 'z': value = (unsigned long long)va_arg(vargs, size_t); break;
                    case 't': value = (unsigned long long)va_arg(vargs, ptrdiff_t); break;
                    case 'j': value = (unsigned long long)va_arg(vargs, uintmax_t); break;
                    default: value = va_arg(vargs, unsigned int); break;
                }
                const char *number_format = *cursor == 'u' ? "%llu" : *cursor == 'x' ? "%llx" : "%llX";
                if (_PyPon_FormatAppendUnsigned(out, capacity, &used, number_format, value) < 0) {
                    return -1;
                }
            } else {
                PyErr_SetString(PyExc_ValueError, "unsupported PyUnicode_FromFormat length modifier");
                return -1;
            }
        } else if (*cursor == 'c') {
            char ch = (char)va_arg(vargs, int);
            if (_PyPon_FormatAppend(out, capacity, &used, &ch, 1) < 0) {
                return -1;
            }
        } else if (*cursor == 'S') {
            PyObject *object = va_arg(vargs, PyObject *);
            if (_PyPon_FormatAppendObject(out, capacity, &used, object, 0) < 0) {
                return -1;
            }
        } else if (*cursor == 'R') {
            PyObject *object = va_arg(vargs, PyObject *);
            if (_PyPon_FormatAppendObject(out, capacity, &used, object, 1) < 0) {
                return -1;
            }
        } else {
            PyErr_SetString(PyExc_ValueError, "unsupported PyUnicode_FromFormat format code");
            return -1;
        }
        cursor++;
    }

    if (out != NULL) {
        if (capacity <= used) {
            PyErr_SetString(PyExc_ValueError, "PyUnicode_FromFormat render buffer is too small");
            return -1;
        }
        out[used] = '\0';
    }
    return used;
}

static inline PyObject *PyUnicode_FromFormatV(const char *format, va_list vargs) {
    if (format == NULL) {
        PyErr_SetString(PyExc_ValueError, "PyUnicode_FromFormatV received NULL format");
        return NULL;
    }

    va_list measure_args;
    va_copy(measure_args, vargs);
    Py_ssize_t needed = _PyPon_FormatUnicodeInto(NULL, 0, format, measure_args);
    va_end(measure_args);
    if (needed < 0) {
        return NULL;
    }

    char stack[512];
    char *buffer = stack;
    if (needed >= (Py_ssize_t)sizeof(stack)) {
        buffer = (char *)malloc((size_t)needed + 1);
        if (buffer == NULL) {
            PyErr_SetString(PyExc_RuntimeError, "PyUnicode_FromFormat allocation failed");
            return NULL;
        }
    }

    va_list render_args;
    va_copy(render_args, vargs);
    Py_ssize_t written = _PyPon_FormatUnicodeInto(buffer, needed + 1, format, render_args);
    va_end(render_args);
    if (written < 0) {
        if (buffer != stack) {
            free(buffer);
        }
        return NULL;
    }

    PyObject *result = PyPon_Capi()->strings->unicode_from_utf8(buffer, written);
    if (buffer != stack) {
        free(buffer);
    }
    return result;
}

static inline PyObject *PyUnicode_FromFormat(const char *format, ...) {
    va_list vargs;
    va_start(vargs, format);
    PyObject *result = PyUnicode_FromFormatV(format, vargs);
    va_end(vargs);
    return result;
}

#endif /* PON_CAPI_STRINGS_INLINE_H */
