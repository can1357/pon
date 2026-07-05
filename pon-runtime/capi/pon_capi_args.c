#include <Python.h>

#include <limits.h>
#include <stdarg.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define PON_ARG_MAX_UNITS 128

typedef enum PyPonArgModifier {
    PON_ARG_MOD_NONE = 0,
    PON_ARG_MOD_HASH,
    PON_ARG_MOD_STAR,
    PON_ARG_MOD_TYPE,
    PON_ARG_MOD_CONVERTER,
} PyPonArgModifier;

typedef struct PyPonArgUnit {
    char code;
    PyPonArgModifier modifier;
    int optional;
    int kwonly;
} PyPonArgUnit;

typedef struct PyPonArgFormat {
    PyPonArgUnit units[PON_ARG_MAX_UNITS];
    Py_ssize_t count;
    Py_ssize_t min_count;
    Py_ssize_t max_positional;
    const char *function_name;
    const char *custom_message;
} PyPonArgFormat;

typedef struct PyPonArgContext {
    const char *function_name;
    const char *custom_message;
} PyPonArgContext;

typedef struct PyPonArgDest {
    void *primary;
    void *secondary;
    PyTypeObject *type;
    PyPonArgConverter converter;
} PyPonArgDest;

typedef struct PyPonObjectVec {
    PyObject **items;
    Py_ssize_t len;
    Py_ssize_t cap;
} PyPonObjectVec;

static int pypon_arg_is_type(PyObject *object, PyTypeObject *type) {
    if (object == NULL || type == NULL) {
        return 0;
    }
    PyTypeObject *actual = Py_TYPE(object);
    while (actual != NULL) {
        if (actual == type) {
            return 1;
        }
        actual = actual->tp_base;
    }
    return 0;
}

static const char *pypon_arg_type_name(PyObject *object) {
    if (object == NULL) {
        return "NULL";
    }
    PyTypeObject *type = Py_TYPE(object);
    if (type == NULL || type->tp_name == NULL) {
        return "object";
    }
    return type->tp_name;
}

static const char *pypon_expected_type_name(PyTypeObject *type) {
    if (type == NULL || type->tp_name == NULL) {
        return "object";
    }
    return type->tp_name;
}

static int pypon_set_custom_error(const PyPonArgContext *ctx) {
    if (ctx != NULL && ctx->custom_message != NULL) {
        PyErr_Clear();
        PyErr_SetString(PyExc_TypeError, ctx->custom_message);
        return 1;
    }
    return 0;
}

static void pypon_set_type_error(const PyPonArgContext *ctx, Py_ssize_t index, const char *expected, PyObject *object) {
    if (pypon_set_custom_error(ctx)) {
        return;
    }
    char message[512];
    if (ctx != NULL && ctx->function_name != NULL && ctx->function_name[0] != '\0') {
        snprintf(message, sizeof(message), "%s() argument %zd must be %s, not %s", ctx->function_name, index, expected, pypon_arg_type_name(object));
    } else {
        snprintf(message, sizeof(message), "argument %zd must be %s, not %s", index, expected, pypon_arg_type_name(object));
    }
    PyErr_SetString(PyExc_TypeError, message);
}

static const char *pypon_argument_word(Py_ssize_t count) {
    return count == 1 ? "argument" : "arguments";
}

static void pypon_set_arity_error(const PyPonArgContext *ctx, Py_ssize_t min, Py_ssize_t max, Py_ssize_t given) {
    if (pypon_set_custom_error(ctx)) {
        return;
    }
    const char *name = (ctx != NULL && ctx->function_name != NULL && ctx->function_name[0] != '\0')
        ? ctx->function_name
        : "function";
    const int has_function_name = strcmp(name, "function") != 0;
    char subject[256];
    if (has_function_name) {
        snprintf(subject, sizeof(subject), "%s()", name);
    } else {
        snprintf(subject, sizeof(subject), "%s", name);
    }

    char message[512];
    if (min == 0 && max == 0) {
        snprintf(message, sizeof(message), "%s takes no arguments (%zd given)", subject, given);
    } else if (min == max) {
        snprintf(
            message,
            sizeof(message),
            "%s takes exactly %zd %s (%zd given)",
            subject,
            min,
            pypon_argument_word(min),
            given);
    } else if (given < min) {
        snprintf(
            message,
            sizeof(message),
            "%s takes at least %zd %s (%zd given)",
            subject,
            min,
            pypon_argument_word(min),
            given);
    } else {
        snprintf(
            message,
            sizeof(message),
            "%s takes at most %zd %s (%zd given)",
            subject,
            max,
            pypon_argument_word(max),
            given);
    }
    PyErr_SetString(PyExc_TypeError, message);
}

static void pypon_set_positional_arity_error(const PyPonArgContext *ctx, Py_ssize_t max, Py_ssize_t given) {
    if (pypon_set_custom_error(ctx)) {
        return;
    }
    const char *name = (ctx != NULL && ctx->function_name != NULL && ctx->function_name[0] != '\0')
        ? ctx->function_name
        : "function";
    char subject[256];
    if (strcmp(name, "function") != 0) {
        snprintf(subject, sizeof(subject), "%s()", name);
    } else {
        snprintf(subject, sizeof(subject), "%s", name);
    }
    char message[512];
    snprintf(
        message,
        sizeof(message),
        "%s takes at most %zd positional %s (%zd given)",
        subject,
        max,
        pypon_argument_word(max),
        given);
    PyErr_SetString(PyExc_TypeError, message);
}

static void pypon_set_missing_keyword_error(const PyPonArgContext *ctx, const char *name) {
    if (pypon_set_custom_error(ctx)) {
        return;
    }
    char message[512];
    snprintf(message, sizeof(message), "missing required argument '%s'", name == NULL ? "?" : name);
    PyErr_SetString(PyExc_TypeError, message);
}

static void pypon_set_duplicate_keyword_error(const PyPonArgContext *ctx, const char *name, Py_ssize_t position) {
    if (pypon_set_custom_error(ctx)) {
        return;
    }
    char message[512];
    snprintf(
        message,
        sizeof(message),
        "argument given by name ('%s') and position (%zd)",
        name == NULL ? "?" : name,
        position);
    PyErr_SetString(PyExc_TypeError, message);
}

static int pypon_unsupported_format(const PyPonArgContext *ctx, char code) {
    if (pypon_set_custom_error(ctx)) {
        return 0;
    }
    char message[256];
    snprintf(message, sizeof(message), "PyArg_ParseTuple format code '%c' is not supported by Pon", code);
    PyErr_SetString(PyExc_TypeError, message);
    return 0;
}

static int pypon_parse_format(const char *format, int allow_keywords, PyPonArgFormat *out) {
    if (format == NULL || out == NULL) {
        PyErr_SetString(PyExc_TypeError, "argument format must not be NULL");
        return 0;
    }
    memset(out, 0, sizeof(*out));
    out->max_positional = -1;

    int optional = 0;
    int kwonly = 0;
    const char *p = format;
    while (*p != '\0' && *p != ':' && *p != ';') {
        char code = *p++;
        if (code == ' ' || code == '\t' || code == '\n' || code == ',') {
            continue;
        }
        if (code == '|') {
            optional = 1;
            continue;
        }
        if (code == '$') {
            if (!allow_keywords) {
                PyErr_SetString(PyExc_TypeError, "'$' in format is only valid for keyword parsing");
                return 0;
            }
            kwonly = 1;
            optional = 1;
            if (out->max_positional < 0) {
                out->max_positional = out->count;
            }
            continue;
        }
        if (out->count >= PON_ARG_MAX_UNITS) {
            PyErr_SetString(PyExc_TypeError, "too many PyArg format units");
            return 0;
        }
        PyPonArgModifier modifier = PON_ARG_MOD_NONE;
        if (*p == '#') {
            modifier = PON_ARG_MOD_HASH;
            p++;
        } else if (*p == '*') {
            modifier = PON_ARG_MOD_STAR;
            p++;
        } else if (*p == '!') {
            modifier = PON_ARG_MOD_TYPE;
            p++;
        } else if (*p == '&') {
            modifier = PON_ARG_MOD_CONVERTER;
            p++;
        }

        int supported = 0;
        switch (code) {
            case 'i': case 'l': case 'L': case 'n': case 'I': case 'k': case 'K':
            case 'h': case 'H': case 'b': case 'B': case 'c': case 'C': case 'f':
            case 'd': case 'p': case 'U':
                supported = modifier == PON_ARG_MOD_NONE;
                break;
            case 's': case 'z': case 'y':
                supported = modifier == PON_ARG_MOD_NONE || modifier == PON_ARG_MOD_HASH || modifier == PON_ARG_MOD_STAR;
                break;
            case 'O':
                supported = modifier == PON_ARG_MOD_NONE || modifier == PON_ARG_MOD_TYPE || modifier == PON_ARG_MOD_CONVERTER;
                break;
            case 'u':
                PyErr_SetString(PyExc_TypeError, "PyArg_ParseTuple format code 'u' was removed in Python 3.12");
                return 0;
            case 'e':
            case 'w':
                return pypon_unsupported_format(&(PyPonArgContext){ out->function_name, out->custom_message }, code);
            default:
                return pypon_unsupported_format(&(PyPonArgContext){ out->function_name, out->custom_message }, code);
        }
        if (!supported) {
            char message[256];
            snprintf(message, sizeof(message), "invalid PyArg format unit '%c'", code);
            PyErr_SetString(PyExc_TypeError, message);
            return 0;
        }
        out->units[out->count++] = (PyPonArgUnit){ code, modifier, optional, kwonly };
        if (!optional) {
            out->min_count = out->count;
        }
    }
    if (out->max_positional < 0) {
        out->max_positional = out->count;
    }
    if (*p == ':') {
        out->function_name = p + 1;
    } else if (*p == ';') {
        out->custom_message = p + 1;
    }
    return 1;
}

static int pypon_consume_destinations(const PyPonArgFormat *format, va_list *vargs, PyPonArgDest *dests) {
    for (Py_ssize_t i = 0; i < format->count; i++) {
        PyPonArgUnit unit = format->units[i];
        PyPonArgDest *dest = &dests[i];
        memset(dest, 0, sizeof(*dest));
        switch (unit.code) {
            case 'O':
                if (unit.modifier == PON_ARG_MOD_TYPE) {
                    dest->type = va_arg(*vargs, PyTypeObject *);
                    dest->primary = va_arg(*vargs, PyObject **);
                } else if (unit.modifier == PON_ARG_MOD_CONVERTER) {
                    dest->converter = va_arg(*vargs, PyPonArgConverter);
                    dest->primary = va_arg(*vargs, void *);
                } else {
                    dest->primary = va_arg(*vargs, PyObject **);
                }
                break;
            case 's':
            case 'z':
            case 'y':
                if (unit.modifier == PON_ARG_MOD_HASH) {
                    dest->primary = va_arg(*vargs, char **);
                    dest->secondary = va_arg(*vargs, Py_ssize_t *);
                } else if (unit.modifier == PON_ARG_MOD_STAR) {
                    dest->primary = va_arg(*vargs, Py_buffer *);
                } else {
                    dest->primary = va_arg(*vargs, char **);
                }
                break;
            default:
                dest->primary = va_arg(*vargs, void *);
                break;
        }
    }
    return 1;
}

static int pypon_checked_long_long(PyObject *object, long long *out, const PyPonArgContext *ctx) {
    long long value = PyLong_AsLongLong(object);
    if (value == -1 && PyErr_Occurred()) {
        pypon_set_custom_error(ctx);
        return 0;
    }
    *out = value;
    return 1;
}

static int pypon_checked_unsigned_long_long(PyObject *object, unsigned long long *out, const PyPonArgContext *ctx) {
    size_t value = PyLong_AsSize_t(object);
    if (value == (size_t)-1 && PyErr_Occurred()) {
        pypon_set_custom_error(ctx);
        return 0;
    }
    *out = value;
    return 1;
}

static int pypon_range_error(const PyPonArgContext *ctx, Py_ssize_t index) {
    if (pypon_set_custom_error(ctx)) {
        return 0;
    }
    char message[256];
    snprintf(message, sizeof(message), "argument %zd out of range", index);
#ifdef PyExc_OverflowError
    PyErr_SetString(PyExc_OverflowError, message);
#else
    PyErr_SetString(PyExc_ValueError, message);
#endif
    return 0;
}

static int pypon_get_text_or_bytes(
    PyObject *object,
    int allow_none,
    int allow_unicode,
    int allow_bytes,
    char **buffer,
    Py_ssize_t *length,
    const PyPonArgContext *ctx,
    Py_ssize_t index,
    const char *expected) {
    if (allow_none && object == Py_None) {
        *buffer = NULL;
        if (length != NULL) {
            *length = 0;
        }
        return 1;
    }
    if (allow_unicode && pypon_arg_is_type(object, &PyUnicode_Type)) {
        Py_ssize_t utf8_length = 0;
        const char *text = PyUnicode_AsUTF8AndSize(object, &utf8_length);
        if (text == NULL) {
            pypon_set_custom_error(ctx);
            return 0;
        }
        *buffer = (char *)text;
        if (length != NULL) {
            *length = utf8_length;
        }
        return 1;
    }
    if (allow_bytes && pypon_arg_is_type(object, &PyBytes_Type)) {
        if (PyBytes_AsStringAndSize(object, buffer, length) < 0) {
            pypon_set_custom_error(ctx);
            return 0;
        }
        return 1;
    }
    if (allow_bytes && pypon_arg_is_type(object, &PyByteArray_Type)) {
        char *bytes = PyByteArray_AsString(object);
        if (bytes == NULL) {
            pypon_set_custom_error(ctx);
            return 0;
        }
        Py_ssize_t size = PyByteArray_Size(object);
        if (size < 0) {
            pypon_set_custom_error(ctx);
            return 0;
        }
        *buffer = bytes;
        if (length != NULL) {
            *length = size;
        }
        return 1;
    }
    pypon_set_type_error(ctx, index, expected, object);
    return 0;
}

static int pypon_no_embedded_nul(const char *buffer, Py_ssize_t length, const PyPonArgContext *ctx) {
    if (buffer != NULL && length > 0 && memchr(buffer, '\0', (size_t)length) != NULL) {
        if (!pypon_set_custom_error(ctx)) {
            PyErr_SetString(PyExc_ValueError, "embedded null byte");
        }
        return 0;
    }
    return 1;
}

static int pypon_decode_one_utf8(const char *buffer, Py_ssize_t length, Py_UCS4 *out) {
    if (buffer == NULL || length <= 0) {
        return 0;
    }
    const unsigned char *s = (const unsigned char *)buffer;
    Py_UCS4 value = 0;
    Py_ssize_t used = 0;
    if (s[0] < 0x80) {
        value = s[0];
        used = 1;
    } else if ((s[0] & 0xe0) == 0xc0 && length >= 2) {
        value = ((Py_UCS4)(s[0] & 0x1f) << 6) | (Py_UCS4)(s[1] & 0x3f);
        used = 2;
    } else if ((s[0] & 0xf0) == 0xe0 && length >= 3) {
        value = ((Py_UCS4)(s[0] & 0x0f) << 12) | ((Py_UCS4)(s[1] & 0x3f) << 6) | (Py_UCS4)(s[2] & 0x3f);
        used = 3;
    } else if ((s[0] & 0xf8) == 0xf0 && length >= 4) {
        value = ((Py_UCS4)(s[0] & 0x07) << 18)
            | ((Py_UCS4)(s[1] & 0x3f) << 12)
            | ((Py_UCS4)(s[2] & 0x3f) << 6)
            | (Py_UCS4)(s[3] & 0x3f);
        used = 4;
    } else {
        return 0;
    }
    if (used != length) {
        return 0;
    }
    *out = value;
    return 1;
}

static Py_ssize_t pypon_encode_ucs4(Py_UCS4 value, char out[4]) {
    if (value <= 0x7f) {
        out[0] = (char)value;
        return 1;
    }
    if (value <= 0x7ff) {
        out[0] = (char)(0xc0 | (value >> 6));
        out[1] = (char)(0x80 | (value & 0x3f));
        return 2;
    }
    if (value <= 0xffff) {
        out[0] = (char)(0xe0 | (value >> 12));
        out[1] = (char)(0x80 | ((value >> 6) & 0x3f));
        out[2] = (char)(0x80 | (value & 0x3f));
        return 3;
    }
    if (value <= 0x10ffff) {
        out[0] = (char)(0xf0 | (value >> 18));
        out[1] = (char)(0x80 | ((value >> 12) & 0x3f));
        out[2] = (char)(0x80 | ((value >> 6) & 0x3f));
        out[3] = (char)(0x80 | (value & 0x3f));
        return 4;
    }
    return -1;
}

static int pypon_fill_buffer(PyObject *object, Py_buffer *view, int allow_unicode, const PyPonArgContext *ctx, Py_ssize_t index) {
    if (view == NULL) {
        PyErr_SetString(PyExc_TypeError, "Py_buffer destination must not be NULL");
        return 0;
    }
    char *buffer = NULL;
    Py_ssize_t length = 0;
    const char *expected = allow_unicode ? "str or bytes-like object" : "bytes-like object";
    if (!pypon_get_text_or_bytes(object, 0, allow_unicode, 1, &buffer, &length, ctx, index, expected)) {
        return 0;
    }
    view->buf = buffer;
    view->obj = object;
    view->len = length;
    view->itemsize = 1;
    view->readonly = pypon_arg_is_type(object, &PyByteArray_Type) ? 0 : 1;
    view->ndim = 1;
    view->format = NULL;
    view->shape = NULL;
    view->strides = NULL;
    view->suboffsets = NULL;
    view->internal = NULL;
    return 1;
}

static int pypon_convert_one(
    const PyPonArgUnit *unit,
    const PyPonArgDest *dest,
    PyObject *object,
    Py_ssize_t index,
    const PyPonArgContext *ctx) {
    switch (unit->code) {
        case 'b': {
            long long value;
            if (!pypon_checked_long_long(object, &value, ctx)) return 0;
            if (value < 0 || value > UCHAR_MAX) return pypon_range_error(ctx, index);
            *(unsigned char *)dest->primary = (unsigned char)value;
            return 1;
        }
        case 'B': {
            unsigned long long value;
            if (!pypon_checked_unsigned_long_long(object, &value, ctx)) return 0;
            if (value > UCHAR_MAX) return pypon_range_error(ctx, index);
            *(unsigned char *)dest->primary = (unsigned char)value;
            return 1;
        }
        case 'h': {
            long long value;
            if (!pypon_checked_long_long(object, &value, ctx)) return 0;
            if (value < SHRT_MIN || value > SHRT_MAX) return pypon_range_error(ctx, index);
            *(short *)dest->primary = (short)value;
            return 1;
        }
        case 'H': {
            unsigned long long value;
            if (!pypon_checked_unsigned_long_long(object, &value, ctx)) return 0;
            if (value > USHRT_MAX) return pypon_range_error(ctx, index);
            *(unsigned short *)dest->primary = (unsigned short)value;
            return 1;
        }
        case 'i': {
            long long value;
            if (!pypon_checked_long_long(object, &value, ctx)) return 0;
            if (value < INT_MIN || value > INT_MAX) return pypon_range_error(ctx, index);
            *(int *)dest->primary = (int)value;
            return 1;
        }
        case 'I': {
            unsigned long long value;
            if (!pypon_checked_unsigned_long_long(object, &value, ctx)) return 0;
            if (value > UINT_MAX) return pypon_range_error(ctx, index);
            *(unsigned int *)dest->primary = (unsigned int)value;
            return 1;
        }
        case 'l': {
            long long value;
            if (!pypon_checked_long_long(object, &value, ctx)) return 0;
            if (value < LONG_MIN || value > LONG_MAX) return pypon_range_error(ctx, index);
            *(long *)dest->primary = (long)value;
            return 1;
        }
        case 'k': {
            unsigned long long value;
            if (!pypon_checked_unsigned_long_long(object, &value, ctx)) return 0;
            if (value > ULONG_MAX) return pypon_range_error(ctx, index);
            *(unsigned long *)dest->primary = (unsigned long)value;
            return 1;
        }
        case 'L': {
            long long value;
            if (!pypon_checked_long_long(object, &value, ctx)) return 0;
            *(long long *)dest->primary = value;
            return 1;
        }
        case 'K': {
            unsigned long long value;
            if (!pypon_checked_unsigned_long_long(object, &value, ctx)) return 0;
            *(unsigned long long *)dest->primary = value;
            return 1;
        }
        case 'n': {
            Py_ssize_t value = PyLong_AsSsize_t(object);
            if (value == (Py_ssize_t)-1 && PyErr_Occurred()) {
                pypon_set_custom_error(ctx);
                return 0;
            }
            *(Py_ssize_t *)dest->primary = value;
            return 1;
        }
        case 'f': {
            double value = PyFloat_AsDouble(object);
            if (value == -1.0 && PyErr_Occurred()) {
                pypon_set_custom_error(ctx);
                return 0;
            }
            *(float *)dest->primary = (float)value;
            return 1;
        }
        case 'd': {
            double value = PyFloat_AsDouble(object);
            if (value == -1.0 && PyErr_Occurred()) {
                pypon_set_custom_error(ctx);
                return 0;
            }
            *(double *)dest->primary = value;
            return 1;
        }
        case 'p': {
            int value = PyObject_IsTrue(object);
            if (value < 0) {
                pypon_set_custom_error(ctx);
                return 0;
            }
            *(int *)dest->primary = value;
            return 1;
        }
        case 'c': {
            char *buffer = NULL;
            Py_ssize_t length = 0;
            if (!pypon_get_text_or_bytes(object, 0, 0, 1, &buffer, &length, ctx, index, "bytes-like object of length 1")) {
                return 0;
            }
            if (length != 1) {
                pypon_set_type_error(ctx, index, "bytes-like object of length 1", object);
                return 0;
            }
            *(char *)dest->primary = buffer[0];
            return 1;
        }
        case 'C': {
            char *buffer = NULL;
            Py_ssize_t length = 0;
            Py_UCS4 value = 0;
            if (!pypon_get_text_or_bytes(object, 0, 1, 0, &buffer, &length, ctx, index, "str of length 1")) {
                return 0;
            }
            if (!pypon_decode_one_utf8(buffer, length, &value)) {
                pypon_set_type_error(ctx, index, "str of length 1", object);
                return 0;
            }
            *(Py_UCS4 *)dest->primary = value;
            return 1;
        }
        case 's':
        case 'z':
        case 'y': {
            int allow_none = unit->code == 'z';
            int allow_unicode = unit->code != 'y';
            int allow_bytes = unit->code == 'y' || unit->modifier == PON_ARG_MOD_HASH || unit->modifier == PON_ARG_MOD_STAR;
            const char *expected = allow_bytes && allow_unicode ? "str or bytes-like object" : (unit->code == 'y' ? "bytes-like object" : "str");
            if (unit->modifier == PON_ARG_MOD_STAR) {
                return pypon_fill_buffer(object, (Py_buffer *)dest->primary, allow_unicode, ctx, index);
            }
            char *buffer = NULL;
            Py_ssize_t length = 0;
            if (!pypon_get_text_or_bytes(object, allow_none, allow_unicode, allow_bytes, &buffer, &length, ctx, index, expected)) {
                return 0;
            }
            if (unit->modifier == PON_ARG_MOD_HASH) {
                *(char **)dest->primary = buffer;
                *(Py_ssize_t *)dest->secondary = length;
                return 1;
            }
            if (!pypon_no_embedded_nul(buffer, length, ctx)) {
                return 0;
            }
            *(char **)dest->primary = buffer;
            return 1;
        }
        case 'U':
            if (!pypon_arg_is_type(object, &PyUnicode_Type)) {
                pypon_set_type_error(ctx, index, "str", object);
                return 0;
            }
            *(PyObject **)dest->primary = object;
            return 1;
        case 'O':
            if (unit->modifier == PON_ARG_MOD_TYPE) {
                if (!pypon_arg_is_type(object, dest->type)) {
                    pypon_set_type_error(ctx, index, pypon_expected_type_name(dest->type), object);
                    return 0;
                }
                *(PyObject **)dest->primary = object;
                return 1;
            }
            if (unit->modifier == PON_ARG_MOD_CONVERTER) {
                if (dest->converter == NULL || !dest->converter(object, dest->primary)) {
                    if (!PyErr_Occurred()) {
                        pypon_set_type_error(ctx, index, "convertible object", object);
                    } else {
                        pypon_set_custom_error(ctx);
                    }
                    return 0;
                }
                return 1;
            }
            *(PyObject **)dest->primary = object;
            return 1;
        default:
            return pypon_unsupported_format(ctx, unit->code);
    }
}

static int pypon_parse_collected(
    PyObject *args,
    PyObject *kwargs,
    const PyPonArgFormat *format,
    char **kwlist,
    const PyPonArgDest *dests,
    int allow_keywords) {
    PyPonArgContext ctx = { format->function_name, format->custom_message };
    Py_ssize_t argc = PyTuple_Size(args);
    if (argc < 0) {
        pypon_set_custom_error(&ctx);
        return 0;
    }
    if (argc > format->max_positional) {
        if (allow_keywords && format->max_positional < format->count) {
            pypon_set_positional_arity_error(&ctx, format->max_positional, argc);
        } else {
            pypon_set_arity_error(&ctx, format->min_count, format->count, argc);
        }
        return 0;
    }

    PyObject *values[PON_ARG_MAX_UNITS];
    int present[PON_ARG_MAX_UNITS];
    memset(values, 0, sizeof(values));
    memset(present, 0, sizeof(present));

    for (Py_ssize_t i = 0; i < argc; i++) {
        PyObject *item = PyTuple_GetItem(args, i);
        if (item == NULL) {
            pypon_set_custom_error(&ctx);
            return 0;
        }
        values[i] = item;
        present[i] = 1;
    }

    Py_ssize_t matched_keywords = 0;
    if (allow_keywords && kwargs != NULL && kwargs != Py_None) {
        Py_ssize_t kwargs_size = PyDict_Size(kwargs);
        if (kwargs_size < 0) {
            pypon_set_custom_error(&ctx);
            return 0;
        }
        for (Py_ssize_t i = 0; i < format->count; i++) {
            if (kwlist == NULL || kwlist[i] == NULL) {
                PyErr_SetString(PyExc_TypeError, "more argument specifiers than keyword list entries");
                return 0;
            }
            if (kwlist[i][0] == '\0') {
                continue;
            }
            PyObject *keyword_value = PyDict_GetItemString(kwargs, kwlist[i]);
            if (keyword_value != NULL) {
                matched_keywords++;
                if (present[i]) {
                    pypon_set_duplicate_keyword_error(&ctx, kwlist[i], i + 1);
                    return 0;
                }
                values[i] = keyword_value;
                present[i] = 1;
            }
        }
        if (kwargs_size > matched_keywords) {
            if (!pypon_set_custom_error(&ctx)) {
                const char *name = (ctx.function_name != NULL && ctx.function_name[0] != '\0') ? ctx.function_name : "function";
                char message[512];
                if (strcmp(name, "function") != 0) {
                    snprintf(message, sizeof(message), "%s() got an unexpected keyword argument", name);
                } else {
                    snprintf(message, sizeof(message), "%s got an unexpected keyword argument", name);
                }
                PyErr_SetString(PyExc_TypeError, message);
            }
            return 0;
        }
    }

    for (Py_ssize_t i = 0; i < format->count; i++) {
        if (!present[i]) {
            if (i < format->min_count) {
                if (allow_keywords && format->units[i].kwonly && kwlist != NULL && kwlist[i] != NULL) {
                    pypon_set_missing_keyword_error(&ctx, kwlist[i]);
                } else {
                    pypon_set_arity_error(&ctx, format->min_count, format->count, argc);
                }
                return 0;
            }
            continue;
        }
        if (!pypon_convert_one(&format->units[i], &dests[i], values[i], i + 1, &ctx)) {
            return 0;
        }
    }
    return 1;
}

int PyArg_VaParseTupleAndKeywords(PyObject *args, PyObject *kwargs, const char *format_text, char **kwlist, va_list vargs) {
    PyPonArgFormat format;
    if (!pypon_parse_format(format_text, 1, &format)) {
        return 0;
    }
    PyPonArgDest dests[PON_ARG_MAX_UNITS];
    va_list copy;
    va_copy(copy, vargs);
    int ok = pypon_consume_destinations(&format, &copy, dests)
        && pypon_parse_collected(args, kwargs, &format, kwlist, dests, 1);
    va_end(copy);
    return ok;
}

int PyArg_ParseTupleAndKeywords(PyObject *args, PyObject *kwargs, const char *format, char **kwlist, ...) {
    va_list vargs;
    va_start(vargs, kwlist);
    int ok = PyArg_VaParseTupleAndKeywords(args, kwargs, format, kwlist, vargs);
    va_end(vargs);
    return ok;
}

int PyArg_VaParse(PyObject *args, const char *format_text, va_list vargs) {
    PyPonArgFormat format;
    if (!pypon_parse_format(format_text, 0, &format)) {
        return 0;
    }
    PyPonArgDest dests[PON_ARG_MAX_UNITS];
    va_list copy;
    va_copy(copy, vargs);
    int ok = pypon_consume_destinations(&format, &copy, dests)
        && pypon_parse_collected(args, NULL, &format, NULL, dests, 0);
    va_end(copy);
    return ok;
}

int PyArg_ParseTuple(PyObject *args, const char *format, ...) {
    va_list vargs;
    va_start(vargs, format);
    int ok = PyArg_VaParse(args, format, vargs);
    va_end(vargs);
    return ok;
}

int PyArg_UnpackTuple(PyObject *args, const char *name, Py_ssize_t min, Py_ssize_t max, ...) {
    if (min < 0 || max < min) {
        PyErr_SetString(PyExc_TypeError, "invalid PyArg_UnpackTuple bounds");
        return 0;
    }
    Py_ssize_t argc = PyTuple_Size(args);
    if (argc < 0) {
        return 0;
    }
    va_list vargs;
    va_start(vargs, max);
    PyObject **outputs[PON_ARG_MAX_UNITS];
    if (max > PON_ARG_MAX_UNITS) {
        va_end(vargs);
        PyErr_SetString(PyExc_TypeError, "too many PyArg_UnpackTuple outputs");
        return 0;
    }
    for (Py_ssize_t i = 0; i < max; i++) {
        outputs[i] = va_arg(vargs, PyObject **);
    }
    va_end(vargs);
    if (argc < min || argc > max) {
        const char *display_name = (name == NULL || name[0] == '\0') ? "function" : name;
        char message[512];
        if (min == max) {
            snprintf(message, sizeof(message), "%s expected %zd %s, got %zd", display_name, min, pypon_argument_word(min), argc);
        } else if (argc < min) {
            snprintf(message, sizeof(message), "%s expected at least %zd %s, got %zd", display_name, min, pypon_argument_word(min), argc);
        } else {
            snprintf(message, sizeof(message), "%s expected at most %zd %s, got %zd", display_name, max, pypon_argument_word(max), argc);
        }
        PyErr_SetString(PyExc_TypeError, message);
        return 0;
    }
    for (Py_ssize_t i = 0; i < argc; i++) {
        PyObject *item = PyTuple_GetItem(args, i);
        if (item == NULL) {
            return 0;
        }
        *outputs[i] = item;
    }
    return 1;
}

static int pypon_vec_push(PyPonObjectVec *vec, PyObject *object) {
    if (vec->len == vec->cap) {
        Py_ssize_t new_cap = vec->cap == 0 ? 4 : vec->cap * 2;
        PyObject **items = (PyObject **)realloc(vec->items, (size_t)new_cap * sizeof(PyObject *));
        if (items == NULL) {
            PyErr_SetString(PyExc_RuntimeError, "out of memory in Py_BuildValue");
            return 0;
        }
        vec->items = items;
        vec->cap = new_cap;
    }
    vec->items[vec->len++] = object;
    return 1;
}

static void pypon_vec_clear(PyPonObjectVec *vec) {
    free(vec->items);
    vec->items = NULL;
    vec->len = 0;
    vec->cap = 0;
}

static void pypon_build_skip_separators(const char **format) {
    while (**format == ' ' || **format == '\t' || **format == '\n' || **format == ',') {
        (*format)++;
    }
}

static PyObject *pypon_build_one(const char **format, va_list *vargs);

static PyObject *pypon_build_sequence(const char **format, va_list *vargs, char terminator, int as_list, int force_tuple) {
    PyPonObjectVec vec = { 0 };
    while (**format != '\0' && **format != terminator) {
        pypon_build_skip_separators(format);
        if (**format == terminator || **format == '\0') {
            break;
        }
        PyObject *item = pypon_build_one(format, vargs);
        if (item == NULL) {
            pypon_vec_clear(&vec);
            return NULL;
        }
        if (!pypon_vec_push(&vec, item)) {
            pypon_vec_clear(&vec);
            return NULL;
        }
        pypon_build_skip_separators(format);
    }
    if (terminator != '\0') {
        if (**format != terminator) {
            pypon_vec_clear(&vec);
            PyErr_SetString(PyExc_TypeError, "unmatched Py_BuildValue nesting delimiter");
            return NULL;
        }
        (*format)++;
    }
    if (!as_list && !force_tuple && terminator == '\0' && vec.len == 0) {
        pypon_vec_clear(&vec);
        Py_INCREF(Py_None);
        return Py_None;
    }
    if (!as_list && !force_tuple && terminator == '\0' && vec.len == 1) {
        PyObject *single = vec.items[0];
        pypon_vec_clear(&vec);
        return single;
    }
    PyObject *container = as_list ? PyList_New(vec.len) : PyTuple_New(vec.len);
    if (container == NULL) {
        pypon_vec_clear(&vec);
        return NULL;
    }
    for (Py_ssize_t i = 0; i < vec.len; i++) {
        int status = as_list ? PyList_SetItem(container, i, vec.items[i]) : PyTuple_SetItem(container, i, vec.items[i]);
        if (status < 0) {
            pypon_vec_clear(&vec);
            return NULL;
        }
    }
    pypon_vec_clear(&vec);
    return container;
}

static PyObject *pypon_build_dict(const char **format, va_list *vargs) {
    PyObject *dict = PyDict_New();
    if (dict == NULL) {
        return NULL;
    }
    while (**format != '\0' && **format != '}') {
        pypon_build_skip_separators(format);
        if (**format == '}') {
            break;
        }
        PyObject *key = pypon_build_one(format, vargs);
        if (key == NULL) {
            return NULL;
        }
        pypon_build_skip_separators(format);
        if (**format == ':') {
            (*format)++;
        }
        pypon_build_skip_separators(format);
        PyObject *value = pypon_build_one(format, vargs);
        if (value == NULL) {
            return NULL;
        }
        if (PyDict_SetItem(dict, key, value) < 0) {
            return NULL;
        }
        pypon_build_skip_separators(format);
    }
    if (**format != '}') {
        PyErr_SetString(PyExc_TypeError, "unmatched Py_BuildValue dict delimiter");
        return NULL;
    }
    (*format)++;
    return dict;
}

static PyObject *pypon_build_one(const char **format, va_list *vargs) {
    pypon_build_skip_separators(format);
    char code = *(*format)++;
    switch (code) {
        case '\0':
            PyErr_SetString(PyExc_TypeError, "unexpected end of Py_BuildValue format");
            return NULL;
        case '(':
            return pypon_build_sequence(format, vargs, ')', 0, 1);
        case '[':
            return pypon_build_sequence(format, vargs, ']', 1, 0);
        case '{':
            return pypon_build_dict(format, vargs);
        case 'i':
            return PyLong_FromLong((long)va_arg(*vargs, int));
        case 'l':
            return PyLong_FromLong(va_arg(*vargs, long));
        case 'L':
            return PyLong_FromLongLong(va_arg(*vargs, long long));
        case 'n':
            return PyLong_FromSsize_t(va_arg(*vargs, Py_ssize_t));
        case 'I':
            return PyLong_FromUnsignedLong((unsigned long)va_arg(*vargs, unsigned int));
        case 'k':
            return PyLong_FromUnsignedLong(va_arg(*vargs, unsigned long));
        case 'K':
            return PyLong_FromUnsignedLongLong(va_arg(*vargs, unsigned long long));
        case 'd':
        case 'f':
            return PyFloat_FromDouble(va_arg(*vargs, double));
        case 's': {
            const char *value = va_arg(*vargs, const char *);
            if (**format == '#') {
                (*format)++;
                Py_ssize_t length = va_arg(*vargs, Py_ssize_t);
                return PyUnicode_FromStringAndSize(value, length);
            }
            if (value == NULL) {
                /* CPython: a NULL C string for 's' builds None. */
                Py_INCREF(Py_None);
                return Py_None;
            }
            return PyUnicode_FromString(value);
        }
        case 'z': {
            const char *value = va_arg(*vargs, const char *);
            if (value == NULL) {
                Py_INCREF(Py_None);
                return Py_None;
            }
            return PyUnicode_FromString(value);
        }
        case 'y': {
            const char *value = va_arg(*vargs, const char *);
            if (**format == '#') {
                (*format)++;
                Py_ssize_t length = va_arg(*vargs, Py_ssize_t);
                return PyBytes_FromStringAndSize(value, length);
            }
            if (value == NULL) {
                PyErr_SetString(PyExc_TypeError, "NULL bytes passed to Py_BuildValue");
                return NULL;
            }
            return PyBytes_FromStringAndSize(value, (Py_ssize_t)strlen(value));
        }
        case 'c': {
            char ch = (char)va_arg(*vargs, int);
            return PyBytes_FromStringAndSize(&ch, 1);
        }
        case 'C': {
            Py_UCS4 value = (Py_UCS4)va_arg(*vargs, unsigned int);
            char buffer[4];
            Py_ssize_t length = pypon_encode_ucs4(value, buffer);
            if (length < 0) {
                PyErr_SetString(PyExc_ValueError, "character out of range in Py_BuildValue");
                return NULL;
            }
            return PyUnicode_FromStringAndSize(buffer, length);
        }
        case 'O':
        case 'S':
        case 'N': {
            PyObject *object = va_arg(*vargs, PyObject *);
            if (object == NULL) {
                PyErr_SetString(PyExc_TypeError, "NULL object passed to Py_BuildValue");
                return NULL;
            }
            /* Registered foreign type statics (e.g. PyExc_* twins, &Some_Type)
             * must not leak into pon structures: translate to the native
             * class before the value is stored. */
            object = PyPon_Capi()->core->normalize_foreign(object);
            /* Pon's GC owns object lifetimes.  O and N both pin the object here:
             * CPython's borrow/steal distinction is not observable in this shim. */
            Py_INCREF(object);
            return object;
        }
        default: {
            char message[256];
            snprintf(message, sizeof(message), "Py_BuildValue format code '%c' is not supported by Pon", code);
            PyErr_SetString(PyExc_TypeError, message);
            return NULL;
        }
    }
}

PyObject *Py_VaBuildValue(const char *format, va_list vargs) {
    if (format == NULL) {
        PyErr_SetString(PyExc_TypeError, "Py_BuildValue format must not be NULL");
        return NULL;
    }
    const char *cursor = format;
    va_list copy;
    va_copy(copy, vargs);
    PyObject *result = pypon_build_sequence(&cursor, &copy, '\0', 0, 0);
    va_end(copy);
    if (result == NULL) {
        return NULL;
    }
    pypon_build_skip_separators(&cursor);
    if (*cursor != '\0') {
        PyErr_SetString(PyExc_TypeError, "trailing Py_BuildValue format data");
        return NULL;
    }
    return result;
}

PyObject *Py_BuildValue(const char *format, ...) {
    va_list vargs;
    va_start(vargs, format);
    PyObject *result = Py_VaBuildValue(format, vargs);
    va_end(vargs);
    return result;
}

void PyBuffer_Release(Py_buffer *view) {
    if (view == NULL) {
        return;
    }
    PyObject *obj = view->obj;
    PyPon_Capi()->object_->release_buffer(view);
    Py_XDECREF(obj);
}
