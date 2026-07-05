#ifndef PON_CAPI_OBJECT_H
#define PON_CAPI_OBJECT_H

#include <stdio.h>

/* Object family: attributes, generic calls, object protocol helpers, and
 * type/class predicates. */

typedef struct PyPonCapiObject {
    PyObject *(*get_attr)(PyObject *, PyObject *);
    PyObject *(*get_attr_string)(PyObject *, const char *);
    int (*set_attr)(PyObject *, PyObject *, PyObject *);
    int (*set_attr_string)(PyObject *, const char *, PyObject *);
    int (*has_attr)(PyObject *, PyObject *);
    int (*has_attr_string)(PyObject *, const char *);
    PyObject *(*call)(PyObject *, PyObject *, PyObject *);
    PyObject *(*call_object)(PyObject *, PyObject *);
    PyObject *(*call_no_args)(PyObject *);
    PyObject *(*call_one_arg)(PyObject *, PyObject *);
    PyObject *(*call_varargs)(PyObject *, PyObject *, PyObject **, size_t);
    PyObject *(*repr)(PyObject *);
    PyObject *(*str)(PyObject *);
    int (*is_true)(PyObject *);
    int (*not_)(PyObject *);
    PyObject *(*rich_compare)(PyObject *, PyObject *, int);
    int (*rich_compare_bool)(PyObject *, PyObject *, int);
    PyObject *(*get_item)(PyObject *, PyObject *);
    int (*set_item)(PyObject *, PyObject *, PyObject *);
    int (*del_item)(PyObject *, PyObject *);
    PyObject *(*get_iter)(PyObject *);
    PyObject *(*iter_next)(PyObject *);
    Py_ssize_t (*size)(PyObject *);
    Py_hash_t (*hash)(PyObject *);
    int (*callable_check)(PyObject *);
    int (*is_instance)(PyObject *, PyObject *);
    int (*is_subclass)(PyObject *, PyObject *);
    PyObject *(*type)(PyObject *);
    PyObject *(*self_iter)(PyObject *);
    int (*get_optional_attr)(PyObject *, PyObject *, PyObject **);
    int (*as_file_descriptor)(PyObject *);
    PyObject *(*vectorcall)(PyObject *, PyObject *const *, size_t, PyObject *);
    PyObject *(*vectorcall_dict)(PyObject *, PyObject *const *, size_t, PyObject *);
    PyObject *(*vectorcall_call)(PyObject *, PyObject *, PyObject *);
    void *(*vectorcall_function)(PyObject *);
    int (*get_buffer)(PyObject *, Py_buffer *, int);
    void (*release_buffer)(Py_buffer *);
    int (*buffer_fill_info)(Py_buffer *, PyObject *, void *, Py_ssize_t, int, int);
    int (*buffer_is_contiguous)(const Py_buffer *, char);
    int (*check_buffer)(PyObject *);
    PyObject *(*memoryview_from_object)(PyObject *);
    PyObject *(*memoryview_from_buffer)(const Py_buffer *);
    Py_buffer *(*memoryview_get_buffer)(PyObject *);
    PyObject *(*memoryview_get_base)(PyObject *);
    int (*type_check)(PyObject *);
    int (*iter_check)(PyObject *);
    PyObject *(*generic_get_attr)(PyObject *, PyObject *);
    int (*generic_set_attr)(PyObject *, PyObject *, PyObject *);
    PyObject *(*generic_get_dict)(PyObject *, void *);
    int (*print)(PyObject *, FILE *, int);
    PyObject *(*format)(PyObject *, PyObject *);
    void (*clear_weakrefs)(PyObject *);
    PyObject *(*seq_iter_new)(PyObject *);
    PyObject *(*method_new)(PyObject *, PyObject *);
    /* Family expansion point: append fields only; never reorder. */
} PyPonCapiObject;

#endif /* PON_CAPI_OBJECT_H */
