#ifndef PON_CAPI_OBJECT_H
#define PON_CAPI_OBJECT_H

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
    /* Family expansion point: append fields only; never reorder. */
} PyPonCapiObject;

#endif /* PON_CAPI_OBJECT_H */
