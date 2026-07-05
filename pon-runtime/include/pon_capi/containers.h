#ifndef PON_CAPI_CONTAINERS_H
#define PON_CAPI_CONTAINERS_H

/* Containers family: tuple/list/dict/set/slice and abstract container helpers. */

typedef struct PyPonCapiContainers {
    PyObject *(*tuple_new)(Py_ssize_t);
    Py_ssize_t (*tuple_size)(PyObject *);
    PyObject *(*tuple_get_item)(PyObject *, Py_ssize_t);
    int (*tuple_set_item)(PyObject *, Py_ssize_t, PyObject *);
    PyObject *(*tuple_pack)(PyObject **, Py_ssize_t);
    PyObject *(*tuple_get_slice)(PyObject *, Py_ssize_t, Py_ssize_t);
    PyObject *(*list_new)(Py_ssize_t);
    Py_ssize_t (*list_size)(PyObject *);
    PyObject *(*list_get_item)(PyObject *, Py_ssize_t);
    int (*list_set_item)(PyObject *, Py_ssize_t, PyObject *);
    int (*list_append)(PyObject *, PyObject *);
    int (*list_insert)(PyObject *, Py_ssize_t, PyObject *);
    PyObject *(*list_as_tuple)(PyObject *);
    int (*list_sort)(PyObject *);
    PyObject *(*dict_new)(void);
    int (*dict_set_item)(PyObject *, PyObject *, PyObject *);
    int (*dict_set_item_string)(PyObject *, const char *, PyObject *);
    PyObject *(*dict_get_item)(PyObject *, PyObject *);
    PyObject *(*dict_get_item_string)(PyObject *, const char *);
    PyObject *(*dict_get_item_with_error)(PyObject *, PyObject *);
    int (*dict_del_item)(PyObject *, PyObject *);
    int (*dict_contains)(PyObject *, PyObject *);
    Py_ssize_t (*dict_size)(PyObject *);
    PyObject *(*dict_keys)(PyObject *);
    PyObject *(*dict_values)(PyObject *);
    PyObject *(*dict_items)(PyObject *);
    int (*dict_next)(PyObject *, Py_ssize_t *, PyObject **, PyObject **);
    int (*dict_merge)(PyObject *, PyObject *, int);
    int (*dict_update)(PyObject *, PyObject *);
    PyObject *(*dict_copy)(PyObject *);
    void (*dict_clear)(PyObject *);
    PyObject *(*set_new)(PyObject *);
    int (*set_add)(PyObject *, PyObject *);
    int (*set_contains)(PyObject *, PyObject *);
    Py_ssize_t (*set_size)(PyObject *);
    PyObject *(*slice_new)(PyObject *, PyObject *, PyObject *);
    int (*slice_unpack)(PyObject *, Py_ssize_t *, Py_ssize_t *, Py_ssize_t *);
    Py_ssize_t (*slice_adjust_indices)(Py_ssize_t, Py_ssize_t *, Py_ssize_t *, Py_ssize_t);
    int (*sequence_check)(PyObject *);
    Py_ssize_t (*sequence_size)(PyObject *);
    PyObject *(*sequence_get_item)(PyObject *, Py_ssize_t);
    int (*sequence_set_item)(PyObject *, Py_ssize_t, PyObject *);
    int (*sequence_contains)(PyObject *, PyObject *);
    PyObject *(*sequence_tuple)(PyObject *);
    PyObject *(*sequence_list)(PyObject *);
    PyObject *(*sequence_fast)(PyObject *, const char *);
    PyObject **(*sequence_fast_items)(PyObject *, Py_ssize_t *);
    int (*mapping_check)(PyObject *);
    PyObject *(*mapping_keys)(PyObject *);
    PyObject *(*mapping_get_item_string)(PyObject *, const char *);
    int (*mapping_set_item_string)(PyObject *, const char *, PyObject *);
    /* Family expansion point: append fields only; never reorder. */
} PyPonCapiContainers;

#endif /* PON_CAPI_CONTAINERS_H */
