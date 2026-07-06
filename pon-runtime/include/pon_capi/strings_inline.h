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

static inline PyObject *PyUnicode_FromOrdinal(int ordinal) {
    return PyPon_Capi()->strings->unicode_from_ordinal(ordinal);
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

static inline PyObject *PyUnicode_Substring(PyObject *object, Py_ssize_t start, Py_ssize_t end) {
    return PyPon_Capi()->strings->unicode_substring(object, start, end);
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

static inline PyObject *PyUnicode_Decode(const char *value, Py_ssize_t size, const char *encoding, const char *errors) {
    return PyPon_Capi()->strings->unicode_decode(value, size, encoding, errors);
}

static inline PyObject *PyUnicode_FromEncodedObject(PyObject *object, const char *encoding, const char *errors) {
    return PyPon_Capi()->strings->unicode_from_encoded_object(object, encoding, errors);
}

static inline PyObject *PyUnicode_AsUTF8String(PyObject *object) {
    return PyPon_Capi()->strings->unicode_as_utf8_string(object);
}

static inline PyObject *PyUnicode_AsASCIIString(PyObject *object) {
    return PyPon_Capi()->strings->unicode_as_ascii_string(object);
}

static inline PyObject *PyUnicode_AsLatin1String(PyObject *object) {
    return PyPon_Capi()->strings->unicode_as_latin1_string(object);
}

static inline PyObject *PyUnicode_AsEncodedString(PyObject *object, const char *encoding, const char *errors) {
    return PyPon_Capi()->strings->unicode_as_encoded_string(object, encoding, errors);
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

static inline PyObject *PyUnicode_Format(PyObject *format, PyObject *args) {
    return PyPon_Capi()->strings->unicode_format(format, args);
}

static inline PyObject *PyUnicode_Replace(PyObject *object, PyObject *old, PyObject *replacement, Py_ssize_t maxcount) {
    return PyPon_Capi()->strings->unicode_replace(object, old, replacement, maxcount);
}

static inline Py_ssize_t PyUnicode_Tailmatch(PyObject *object, PyObject *substr, Py_ssize_t start, Py_ssize_t end, int direction) {
    return PyPon_Capi()->strings->unicode_tailmatch(object, substr, start, end, direction);
}

static inline int PyUnicode_Contains(PyObject *container, PyObject *element) {
    return PyPon_Capi()->strings->unicode_contains(container, element);
}

static inline int PyUnicode_Check(PyObject *object) {
    return PyPon_Capi()->strings->unicode_check(object);
}

static inline int PyUnicode_CheckExact(PyObject *object) {
    return PyPon_Capi()->strings->unicode_check_exact(object);
}

/* CPython's compact-unicode data macros expose code-unit views. Pon stores
 * strings as UTF-8 internally, so these wrappers return cached UCS1/UCS2/UCS4
 * views owned by the strings C-API family rather than raw object fields.
 */
#define PyUnicode_1BYTE_KIND 1
#define PyUnicode_2BYTE_KIND 2
#define PyUnicode_4BYTE_KIND 4

static inline PyObject *PyUnicode_FromKindAndData(int kind, const void *data, Py_ssize_t size) {
    return PyPon_Capi()->strings->unicode_from_kind_and_data(kind, data, size);
}

static inline Py_UCS4 *PyUnicode_AsUCS4Copy(PyObject *object) {
    return PyPon_Capi()->strings->unicode_as_ucs4_copy(object);
}

static inline Py_UCS4 *PyUnicode_AsUCS4(PyObject *object, Py_UCS4 *buffer, Py_ssize_t buflen, int copy_null) {
    return PyPon_Capi()->strings->unicode_as_ucs4(object, buffer, buflen, copy_null);
}

static inline Py_ssize_t PyUnicode_GET_LENGTH(PyObject *object) {
    return PyUnicode_GetLength(object);
}

static inline int PyUnicode_KIND(PyObject *object) {
    return PyPon_Capi()->strings->unicode_kind(object);
}

static inline void *PyUnicode_DATA(PyObject *object) {
    return (void *)PyPon_Capi()->strings->unicode_data(object);
}

static inline Py_UCS1 *PyUnicode_1BYTE_DATA(PyObject *object) {
    return (Py_UCS1 *)PyUnicode_DATA(object);
}

static inline Py_UCS2 *PyUnicode_2BYTE_DATA(PyObject *object) {
    return (Py_UCS2 *)PyUnicode_DATA(object);
}

static inline Py_UCS4 *PyUnicode_4BYTE_DATA(PyObject *object) {
    return (Py_UCS4 *)PyUnicode_DATA(object);
}

static inline Py_UCS4 PyUnicode_READ_CHAR(PyObject *object, Py_ssize_t index) {
    return PyPon_Capi()->strings->unicode_read_char(object, index);
}

static inline Py_UCS4 PyUnicode_READ(int kind, const void *data, Py_ssize_t index) {
    if (data == NULL || index < 0) {
        PyErr_SetString(PyExc_SystemError, "PyUnicode_READ received invalid data");
        return (Py_UCS4)-1;
    }
    if (kind == PyUnicode_1BYTE_KIND) {
        return ((const Py_UCS1 *)data)[index];
    }
    if (kind == PyUnicode_2BYTE_KIND) {
        return ((const Py_UCS2 *)data)[index];
    }
    if (kind == PyUnicode_4BYTE_KIND) {
        return ((const Py_UCS4 *)data)[index];
    }
    PyErr_SetString(PyExc_SystemError, "PyUnicode_READ received invalid kind");
    return (Py_UCS4)-1;
}

static inline int PyUnicode_IS_ASCII(PyObject *object) {
    return PyPon_Capi()->strings->unicode_is_ascii(object);
}

#define PyUnicode_CHECK_INTERNED(op) (0)

static inline int PyUnicode_Resize(PyObject **unicode, Py_ssize_t new_size) {
    return PyPon_Capi()->strings->unicode_resize(unicode, new_size);
}

static inline Py_ssize_t PyUnicode_CopyCharacters(PyObject *to, Py_ssize_t to_start, PyObject *from, Py_ssize_t from_start, Py_ssize_t how_many) {
    return PyPon_Capi()->strings->unicode_copy_characters(to, to_start, from, from_start, how_many);
}

static inline PyObject *PyLong_FromUnicodeObject(PyObject *object, int base) {
    return PyPon_Capi()->strings->long_from_unicode_object(object, base);
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

/* CPython fast-path macros: Pon has no exposed bytes layout, so both
 * delegate to the checked accessors. */
#define PyBytes_AS_STRING(op) PyBytes_AsString(op)
#define PyBytes_GET_SIZE(op) PyBytes_Size(op)

static inline void PyBytes_Concat(PyObject **bytes, PyObject *newpart) {
    PyPon_Capi()->strings->bytes_concat(bytes, newpart);
}

static inline int PyBytes_Check(PyObject *object) {
    return PyPon_Capi()->strings->bytes_check(object);
}

static inline int PyBytes_CheckExact(PyObject *object) {
    return PyPon_Capi()->strings->bytes_check_exact(object);
}

/* Pon bytes are immutable runtime objects. _PyBytes_Resize therefore allocates
 * a replacement object and stores it through *bytes; on success callers must
 * treat the old pointer as dead, exactly as CPython callers already do.
 */
static inline int _PyBytes_Resize(PyObject **bytes, Py_ssize_t newsize) {
    if (bytes == NULL || *bytes == NULL || !PyBytes_Check(*bytes)) {
        PyErr_SetString(PyExc_SystemError, "_PyBytes_Resize expected a bytes object");
        return -1;
    }
    if (newsize < 0) {
        PyErr_SetString(PyExc_SystemError, "_PyBytes_Resize received a negative size");
        return -1;
    }

    char *old_buffer = NULL;
    Py_ssize_t old_size = 0;
    if (PyBytes_AsStringAndSize(*bytes, &old_buffer, &old_size) < 0) {
        PyErr_SetString(PyExc_SystemError, "_PyBytes_Resize could not inspect the bytes object");
        return -1;
    }

    PyObject *replacement = NULL;
    if (newsize == 0) {
        replacement = PyBytes_FromStringAndSize("", 0);
    } else {
        char *scratch = (char *)malloc((size_t)newsize);
        if (scratch == NULL) {
            PyErr_NoMemory();
            return -1;
        }
        memset(scratch, 0, (size_t)newsize);
        Py_ssize_t copy = old_size < newsize ? old_size : newsize;
        if (copy > 0) {
            memcpy(scratch, old_buffer, (size_t)copy);
        }
        replacement = PyBytes_FromStringAndSize(scratch, newsize);
        free(scratch);
    }
    if (replacement == NULL) {
        return -1;
    }
    *bytes = replacement;
    return 0;
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

        /* CPython accepts flag/width/precision prefixes ("%.200s", "%5d").
         * Width and flags never change what pon renders (no padding); a
         * precision truncates %s. */
        int has_precision = 0;
        long precision = 0;
        while (*cursor == '-' || *cursor == '+' || *cursor == ' ' || *cursor == '#' || *cursor == '0') {
            cursor++;
        }
        while (*cursor >= '0' && *cursor <= '9') {
            cursor++;
        }
        if (*cursor == '.') {
            cursor++;
            has_precision = 1;
            while (*cursor >= '0' && *cursor <= '9') {
                precision = precision * 10 + (*cursor - '0');
                cursor++;
            }
        }
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
            if (text == NULL) {
                text = "(null)";
            }
            Py_ssize_t text_len = (Py_ssize_t)strlen(text);
            if (has_precision && text_len > (Py_ssize_t)precision) {
                text_len = (Py_ssize_t)precision;
            }
            if (_PyPon_FormatAppend(out, capacity, &used, text, text_len) < 0) {
                return -1;
            }
        } else if (*cursor == 'A') {
            PyObject *object = va_arg(vargs, PyObject *);
            if (_PyPon_FormatAppendObject(out, capacity, &used, object, 1) < 0) {
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

/* ---- PyBytes_FromFormat / PyBytes_FromFormatV ---- */

static inline Py_ssize_t _PyPon_FormatBytesInto(char *out, Py_ssize_t capacity, const char *format, va_list vargs) {
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
            PyErr_SetString(PyExc_ValueError, "incomplete PyBytes_FromFormat format");
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
                PyErr_SetString(PyExc_ValueError, "PyBytes_FromFormat pointer conversion failed");
                return -1;
            }
            if (_PyPon_FormatAppend(out, capacity, &used, stack, (Py_ssize_t)written) < 0) {
                return -1;
            }
        } else if (*cursor == 'c') {
            char ch = (char)va_arg(vargs, int);
            if (_PyPon_FormatAppend(out, capacity, &used, &ch, 1) < 0) {
                return -1;
            }
        } else if (*cursor == 'd' || *cursor == 'i' || *cursor == 'u' || *cursor == 'x'
                   || *cursor == 'l' || *cursor == 'z') {
            char modifier = 0;
            if (*cursor == 'l') {
                modifier = 'l';
                cursor++;
                if (*cursor == 'l') {
                    PyErr_SetString(PyExc_ValueError, "unsupported PyBytes_FromFormat length modifier");
                    return -1;
                }
            } else if (*cursor == 'z') {
                modifier = 'z';
                cursor++;
            }

            if (*cursor == 'd' || *cursor == 'i') {
                long long value;
                if (modifier == 'l') {
                    value = va_arg(vargs, long);
                } else if (modifier == 'z') {
                    value = (long long)va_arg(vargs, Py_ssize_t);
                } else {
                    value = va_arg(vargs, int);
                }
                if (_PyPon_FormatAppendNumber(out, capacity, &used, "%lld", value) < 0) {
                    return -1;
                }
            } else if (*cursor == 'u') {
                unsigned long long value;
                if (modifier == 'l') {
                    value = va_arg(vargs, unsigned long);
                } else if (modifier == 'z') {
                    value = (unsigned long long)va_arg(vargs, size_t);
                } else {
                    value = va_arg(vargs, unsigned int);
                }
                if (_PyPon_FormatAppendUnsigned(out, capacity, &used, "%llu", value) < 0) {
                    return -1;
                }
            } else if (*cursor == 'x' && modifier == 0) {
                unsigned int value = va_arg(vargs, unsigned int);
                if (_PyPon_FormatAppendUnsigned(out, capacity, &used, "%llx", (unsigned long long)value) < 0) {
                    return -1;
                }
            } else {
                PyErr_SetString(PyExc_ValueError, "unsupported PyBytes_FromFormat format code");
                return -1;
            }
        } else {
            PyErr_SetString(PyExc_ValueError, "unsupported PyBytes_FromFormat format code");
            return -1;
        }
        cursor++;
    }

    if (out != NULL) {
        if (capacity <= used) {
            PyErr_SetString(PyExc_ValueError, "PyBytes_FromFormat render buffer is too small");
            return -1;
        }
        out[used] = '\0';
    }
    return used;
}

static inline PyObject *PyBytes_FromFormatV(const char *format, va_list vargs) {
    if (format == NULL) {
        PyErr_SetString(PyExc_ValueError, "PyBytes_FromFormatV received NULL format");
        return NULL;
    }

    va_list measure_args;
    va_copy(measure_args, vargs);
    Py_ssize_t needed = _PyPon_FormatBytesInto(NULL, 0, format, measure_args);
    va_end(measure_args);
    if (needed < 0) {
        return NULL;
    }
    if (needed == PY_SSIZE_T_MAX) {
        PyErr_SetString(PyExc_ValueError, "PyBytes_FromFormat result is too large");
        return NULL;
    }

    char stack[512];
    char *buffer = stack;
    if (needed >= (Py_ssize_t)sizeof(stack)) {
        buffer = (char *)malloc((size_t)needed + 1);
        if (buffer == NULL) {
            PyErr_NoMemory();
            return NULL;
        }
    }

    va_list render_args;
    va_copy(render_args, vargs);
    Py_ssize_t written = _PyPon_FormatBytesInto(buffer, needed + 1, format, render_args);
    va_end(render_args);
    if (written < 0) {
        if (buffer != stack) {
            free(buffer);
        }
        return NULL;
    }

    PyObject *result = PyBytes_FromStringAndSize(buffer, written);
    if (buffer != stack) {
        free(buffer);
    }
    return result;
}

static inline PyObject *PyBytes_FromFormat(const char *format, ...) {
    va_list vargs;
    va_start(vargs, format);
    PyObject *result = PyBytes_FromFormatV(format, vargs);
    va_end(vargs);
    return result;
}

#endif /* PON_CAPI_STRINGS_INLINE_H */
