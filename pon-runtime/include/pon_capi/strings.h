#ifndef PON_CAPI_STRINGS_H
#define PON_CAPI_STRINGS_H

/* Strings family: str/bytes/bytearray construction and extraction. */

typedef struct PyPonCapiStrings {
    PyObject *(*unicode_from_string)(const char *);
    PyObject *(*unicode_from_string_and_size)(const char *, Py_ssize_t);
    const char *(*unicode_as_utf8)(PyObject *);
    const char *(*unicode_as_utf8_and_size)(PyObject *, Py_ssize_t *);
    Py_ssize_t (*unicode_get_length)(PyObject *);
    PyObject *(*unicode_decode_utf8)(const char *, Py_ssize_t, const char *);
    PyObject *(*unicode_decode_ascii)(const char *, Py_ssize_t, const char *);
    PyObject *(*unicode_decode_latin1)(const char *, Py_ssize_t, const char *);
    PyObject *(*unicode_as_utf8_string)(PyObject *);
    PyObject *(*unicode_as_ascii_string)(PyObject *);
    PyObject *(*unicode_intern_from_string)(const char *);
    int (*unicode_compare)(PyObject *, PyObject *);
    int (*unicode_compare_with_ascii_string)(PyObject *, const char *);
    PyObject *(*unicode_concat)(PyObject *, PyObject *);
    PyObject *(*bytes_from_string_and_size)(const char *, Py_ssize_t);
    PyObject *(*bytes_from_string)(const char *);
    Py_ssize_t (*bytes_size)(PyObject *);
    char *(*bytes_as_string)(PyObject *);
    int (*bytes_as_string_and_size)(PyObject *, char **, Py_ssize_t *);
    void (*bytes_concat)(PyObject **, PyObject *);
    PyObject *(*bytearray_from_string_and_size)(const char *, Py_ssize_t);
    Py_ssize_t (*bytearray_size)(PyObject *);
    char *(*bytearray_as_string)(PyObject *);
    int (*unicode_check)(PyObject *);
    int (*unicode_check_exact)(PyObject *);
    int (*bytes_check)(PyObject *);
    int (*bytes_check_exact)(PyObject *);
    int (*bytearray_check)(PyObject *);
    int (*bytearray_check_exact)(PyObject *);

    /* Private helpers for inline wrappers; not CPython API names. */
    PyObject *(*unicode_from_utf8)(const char *, Py_ssize_t);
    PyObject *(*object_str)(PyObject *);
    PyObject *(*object_repr)(PyObject *);
    int (*unicode_kind)(PyObject *);
    const void *(*unicode_data)(PyObject *);
    Py_UCS4 (*unicode_read_char)(PyObject *, Py_ssize_t);
    int (*unicode_is_ascii)(PyObject *);
    PyObject *(*unicode_as_latin1_string)(PyObject *);
    PyObject *(*unicode_from_encoded_object)(PyObject *, const char *, const char *);
    PyObject *(*unicode_from_kind_and_data)(int, const void *, Py_ssize_t);
    Py_UCS4 *(*unicode_as_ucs4_copy)(PyObject *);
    PyObject *(*unicode_as_encoded_string)(PyObject *, const char *, const char *);
    PyObject *(*unicode_format)(PyObject *, PyObject *);
    PyObject *(*unicode_replace)(PyObject *, PyObject *, PyObject *, Py_ssize_t);
    Py_ssize_t (*unicode_tailmatch)(PyObject *, PyObject *, Py_ssize_t, Py_ssize_t, int);
    int (*unicode_contains)(PyObject *, PyObject *);
    PyObject *(*long_from_unicode_object)(PyObject *, int);
    PyObject *(*unicode_substring)(PyObject *, Py_ssize_t, Py_ssize_t);
    Py_UCS4 *(*unicode_as_ucs4)(PyObject *, Py_UCS4 *, Py_ssize_t, int);
    PyObject *(*unicode_from_ordinal)(int);
    PyObject *(*unicode_decode)(const char *, Py_ssize_t, const char *, const char *);
    int (*unicode_resize)(PyObject **, Py_ssize_t);
    Py_ssize_t (*unicode_copy_characters)(PyObject *, Py_ssize_t, PyObject *, Py_ssize_t, Py_ssize_t);
    /* Family expansion point: append fields only; never reorder. */
} PyPonCapiStrings;

#endif /* PON_CAPI_STRINGS_H */
