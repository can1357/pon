#ifndef PON_CAPI_CONTAINERS_INLINE_H
#define PON_CAPI_CONTAINERS_INLINE_H

/* Inline wrapper layer for the containers family. Included by Python.h AFTER
 * the PyPonCapi definition; never include directly.
 */

static inline int _PyPon_IsBuiltinType(PyObject *object, int tid) {
    return object != NULL && PyPon_Capi()->core->builtin_type_id(object) == tid;
}

static inline int PyTuple_Check(PyObject *object) {
    return _PyPon_IsBuiltinType(object, PON_TID_TUPLE);
}

static inline int PyTuple_CheckExact(PyObject *object) {
    return _PyPon_IsBuiltinType(object, PON_TID_TUPLE);
}

static inline int PyList_Check(PyObject *object) {
    return _PyPon_IsBuiltinType(object, PON_TID_LIST);
}

static inline int PyList_CheckExact(PyObject *object) {
    return _PyPon_IsBuiltinType(object, PON_TID_LIST);
}

static inline int PyDict_Check(PyObject *object) {
    return _PyPon_IsBuiltinType(object, PON_TID_DICT);
}

static inline int PyDict_CheckExact(PyObject *object) {
    return _PyPon_IsBuiltinType(object, PON_TID_DICT);
}

static inline int PySet_Check(PyObject *object) {
    return _PyPon_IsBuiltinType(object, PON_TID_SET);
}

static inline int PyFrozenSet_Check(PyObject *object) {
    return _PyPon_IsBuiltinType(object, PON_TID_FROZENSET);
}

static inline int PyAnySet_Check(PyObject *object) {
    return PySet_Check(object) || PyFrozenSet_Check(object);
}

static inline int PySlice_Check(PyObject *object) {
    return _PyPon_IsBuiltinType(object, PON_TID_SLICE);
}

static inline PyObject *PyTuple_New(Py_ssize_t size) {
    return PyPon_Capi()->containers->tuple_new(size);
}

static inline Py_ssize_t PyTuple_Size(PyObject *tuple) {
    return PyPon_Capi()->containers->tuple_size(tuple);
}

static inline Py_ssize_t PyTuple_GET_SIZE(PyObject *tuple) {
    return PyTuple_Size(tuple);
}

static inline PyObject *PyTuple_GetItem(PyObject *tuple, Py_ssize_t index) {
    return PyPon_Capi()->containers->tuple_get_item(tuple, index);
}

#define PyTuple_GET_ITEM(tuple, index) (((PyTupleObject *)(tuple))->items[(index)])

static inline int PyTuple_SetItem(PyObject *tuple, Py_ssize_t index, PyObject *item) {
    return PyPon_Capi()->containers->tuple_set_item(tuple, index, item);
}

#define PyTuple_SET_ITEM(tuple, index, item) (PyPon_Capi()->containers->tuple_set_item((PyObject *)(tuple), (index), (PyObject *)(item)))

static inline PyObject *PyTuple_Pack(Py_ssize_t size, ...) {
    if (size < 0) {
        return NULL;
    }
    PyObject *items[size > 0 ? size : 1];
    va_list args;
    va_start(args, size);
    for (Py_ssize_t i = 0; i < size; i++) {
        items[i] = va_arg(args, PyObject *);
    }
    va_end(args);
    return PyPon_Capi()->containers->tuple_pack(items, size);
}

static inline PyObject *PyTuple_GetSlice(PyObject *tuple, Py_ssize_t start, Py_ssize_t stop) {
    return PyPon_Capi()->containers->tuple_get_slice(tuple, start, stop);
}

static inline PyObject *PyList_New(Py_ssize_t size) {
    return PyPon_Capi()->containers->list_new(size);
}

static inline Py_ssize_t PyList_Size(PyObject *list) {
    return PyPon_Capi()->containers->list_size(list);
}

static inline Py_ssize_t PyList_GET_SIZE(PyObject *list) {
    return PyList_Size(list);
}

static inline PyObject *PyList_GetItem(PyObject *list, Py_ssize_t index) {
    return PyPon_Capi()->containers->list_get_item(list, index);
}

static inline PyObject *PyList_GET_ITEM(PyObject *list, Py_ssize_t index) {
    return PyList_GetItem(list, index);
}

static inline PyObject *PyList_GetItemRef(PyObject *list, Py_ssize_t index) {
    return PyPon_Capi()->containers->list_get_item_ref(list, index);
}

static inline int PyList_SetItem(PyObject *list, Py_ssize_t index, PyObject *item) {
    return PyPon_Capi()->containers->list_set_item(list, index, item);
}

#define PyList_SET_ITEM(list, index, item) (PyPon_Capi()->containers->list_set_item((PyObject *)(list), (index), (PyObject *)(item)))

static inline int PyList_Append(PyObject *list, PyObject *item) {
    return PyPon_Capi()->containers->list_append(list, item);
}

static inline int PyList_Insert(PyObject *list, Py_ssize_t index, PyObject *item) {
    return PyPon_Capi()->containers->list_insert(list, index, item);
}

static inline PyObject *PyList_AsTuple(PyObject *list) {
    return PyPon_Capi()->containers->list_as_tuple(list);
}

static inline int PyList_Sort(PyObject *list) {
    return PyPon_Capi()->containers->list_sort(list);
}

static inline int PyList_SetSlice(PyObject *list, Py_ssize_t low, Py_ssize_t high, PyObject *items) {
    return PyPon_Capi()->containers->list_set_slice(list, low, high, items);
}

static inline PyObject *PyDict_New(void) {
    return PyPon_Capi()->containers->dict_new();
}

static inline int PyDict_SetItem(PyObject *dict, PyObject *key, PyObject *value) {
    return PyPon_Capi()->containers->dict_set_item(dict, key, value);
}

static inline int PyDict_SetItemString(PyObject *dict, const char *key, PyObject *value) {
    return PyPon_Capi()->containers->dict_set_item_string(dict, key, value);
}

static inline PyObject *PyDict_GetItem(PyObject *dict, PyObject *key) {
    return PyPon_Capi()->containers->dict_get_item(dict, key);
}

static inline PyObject *PyDict_GetItemString(PyObject *dict, const char *key) {
    return PyPon_Capi()->containers->dict_get_item_string(dict, key);
}

static inline PyObject *PyDict_GetItemWithError(PyObject *dict, PyObject *key) {
    return PyPon_Capi()->containers->dict_get_item_with_error(dict, key);
}

static inline int PyDict_GetItemRef(PyObject *dict, PyObject *key, PyObject **result) {
    return PyPon_Capi()->containers->dict_get_item_ref(dict, key, result);
}

static inline int PyDict_GetItemStringRef(PyObject *dict, const char *key, PyObject **result) {
    return PyPon_Capi()->containers->dict_get_item_string_ref(dict, key, result);
}

static inline int PyDict_DelItem(PyObject *dict, PyObject *key) {
    return PyPon_Capi()->containers->dict_del_item(dict, key);
}

static inline int PyDict_DelItemString(PyObject *dict, const char *key) {
    return PyPon_Capi()->containers->dict_del_item_string(dict, key);
}

static inline int PyDict_Contains(PyObject *dict, PyObject *key) {
    return PyPon_Capi()->containers->dict_contains(dict, key);
}

static inline int PyDict_ContainsString(PyObject *dict, const char *key) {
    return PyPon_Capi()->containers->dict_contains_string(dict, key);
}

static inline Py_ssize_t PyDict_Size(PyObject *dict) {
    return PyPon_Capi()->containers->dict_size(dict);
}

#define PyDict_GET_SIZE(dict) PyDict_Size((PyObject *)(dict))

static inline int PyDict_SetDefaultRef(PyObject *dict, PyObject *key, PyObject *default_value, PyObject **result) {
    return PyPon_Capi()->containers->dict_set_default_ref(dict, key, default_value, result);
}

static inline PyObject *PyDict_SetDefault(PyObject *dict, PyObject *key, PyObject *default_value) {
    return PyPon_Capi()->containers->dict_set_default(dict, key, default_value);
}

static inline PyObject *PyDict_Keys(PyObject *dict) {
    return PyPon_Capi()->containers->dict_keys(dict);
}

static inline PyObject *PyDict_Values(PyObject *dict) {
    return PyPon_Capi()->containers->dict_values(dict);
}

static inline PyObject *PyDict_Items(PyObject *dict) {
    return PyPon_Capi()->containers->dict_items(dict);
}

static inline int PyDict_Next(PyObject *dict, Py_ssize_t *pos, PyObject **key, PyObject **value) {
    return PyPon_Capi()->containers->dict_next(dict, pos, key, value);
}

static inline int PyDict_Merge(PyObject *dict, PyObject *other, int override) {
    return PyPon_Capi()->containers->dict_merge(dict, other, override);
}

static inline int PyDict_Update(PyObject *dict, PyObject *other) {
    return PyPon_Capi()->containers->dict_update(dict, other);
}

static inline PyObject *PyDict_Copy(PyObject *dict) {
    return PyPon_Capi()->containers->dict_copy(dict);
}

static inline void PyDict_Clear(PyObject *dict) {
    PyPon_Capi()->containers->dict_clear(dict);
}

#define PyDictProxy_Type (*PyPon_Capi()->containers->dict_proxy_type())

static inline int PyDictProxy_Check(PyObject *object) {
    return object != NULL && Py_TYPE(object) == &PyDictProxy_Type;
}

static inline PyObject *PyDictProxy_New(PyObject *mapping) {
    return PyPon_Capi()->containers->dict_proxy_new(mapping);
}

static inline PyObject *PySet_New(PyObject *iterable) {
    return PyPon_Capi()->containers->set_new(iterable);
}

static inline int PySet_Add(PyObject *set, PyObject *item) {
    return PyPon_Capi()->containers->set_add(set, item);
}

static inline int PySet_Contains(PyObject *set, PyObject *item) {
    return PyPon_Capi()->containers->set_contains(set, item);
}

static inline Py_ssize_t PySet_Size(PyObject *set) {
    return PyPon_Capi()->containers->set_size(set);
}

static inline PyObject *PySlice_New(PyObject *start, PyObject *stop, PyObject *step) {
    return PyPon_Capi()->containers->slice_new(start, stop, step);
}

static inline int PySlice_Unpack(PyObject *slice, Py_ssize_t *start, Py_ssize_t *stop, Py_ssize_t *step) {
    return PyPon_Capi()->containers->slice_unpack(slice, start, stop, step);
}

static inline Py_ssize_t PySlice_AdjustIndices(Py_ssize_t length, Py_ssize_t *start, Py_ssize_t *stop, Py_ssize_t step) {
    return PyPon_Capi()->containers->slice_adjust_indices(length, start, stop, step);
}

static inline int PySlice_GetIndicesEx(PyObject *slice, Py_ssize_t length, Py_ssize_t *start, Py_ssize_t *stop, Py_ssize_t *step, Py_ssize_t *slicelength) {
    if (PySlice_Unpack(slice, start, stop, step) < 0) {
        return -1;
    }
    if (slicelength == NULL) {
        PyErr_SetString(PyExc_TypeError, "PySlice_GetIndicesEx received NULL slicelength pointer");
        return -1;
    }
    *slicelength = PySlice_AdjustIndices(length, start, stop, *step);
    return 0;
}

static inline int PySequence_Check(PyObject *object) {
    return PyPon_Capi()->containers->sequence_check(object);
}

static inline Py_ssize_t PySequence_Size(PyObject *object) {
    return PyPon_Capi()->containers->sequence_size(object);
}

static inline Py_ssize_t PySequence_Length(PyObject *object) {
    return PySequence_Size(object);
}

static inline PyObject *PySequence_Concat(PyObject *left, PyObject *right) {
    return PyPon_Capi()->containers->sequence_concat(left, right);
}

static inline PyObject *PySequence_Repeat(PyObject *object, Py_ssize_t count) {
    return PyPon_Capi()->containers->sequence_repeat(object, count);
}

static inline PyObject *PySequence_InPlaceRepeat(PyObject *object, Py_ssize_t count) {
    return PyPon_Capi()->containers->sequence_inplace_repeat(object, count);
}

static inline PyObject *PySequence_InPlaceConcat(PyObject *left, PyObject *right) {
    return PyPon_Capi()->containers->sequence_inplace_concat(left, right);
}

static inline PyObject *PySequence_GetItem(PyObject *object, Py_ssize_t index) {
    return PyPon_Capi()->containers->sequence_get_item(object, index);
}

#define PySequence_ITEM(object, index) PySequence_GetItem((object), (index))

static inline int PySequence_SetItem(PyObject *object, Py_ssize_t index, PyObject *value) {
    return PyPon_Capi()->containers->sequence_set_item(object, index, value);
}

static inline int PySequence_Contains(PyObject *object, PyObject *value) {
    return PyPon_Capi()->containers->sequence_contains(object, value);
}

static inline int PySequence_In(PyObject *object, PyObject *value) {
    return PySequence_Contains(object, value);
}

static inline PyObject *PySequence_Tuple(PyObject *object) {
    return PyPon_Capi()->containers->sequence_tuple(object);
}

static inline PyObject *PySequence_List(PyObject *object) {
    return PyPon_Capi()->containers->sequence_list(object);
}

static inline PyObject *PySequence_Fast(PyObject *object, const char *message) {
    return PyPon_Capi()->containers->sequence_fast(object, message);
}

static inline PyObject **PySequence_Fast_ITEMS(PyObject *object) {
    return PyPon_Capi()->containers->sequence_fast_items(object, NULL);
}

static inline Py_ssize_t PySequence_Fast_GET_SIZE(PyObject *object) {
    Py_ssize_t size = 0;
    (void)PyPon_Capi()->containers->sequence_fast_items(object, &size);
    return size;
}

static inline PyObject *PySequence_Fast_GET_ITEM(PyObject *object, Py_ssize_t index) {
    Py_ssize_t size = 0;
    PyObject **items = PyPon_Capi()->containers->sequence_fast_items(object, &size);
    if (items == NULL || index < 0 || index >= size) {
        return NULL;
    }
    return items[index];
}

static inline int PyMapping_Check(PyObject *object) {
    return PyPon_Capi()->containers->mapping_check(object);
}

static inline PyObject *PyMapping_Keys(PyObject *object) {
    return PyPon_Capi()->containers->mapping_keys(object);
}

static inline PyObject *PyMapping_GetItemString(PyObject *object, const char *key) {
    return PyPon_Capi()->containers->mapping_get_item_string(object, key);
}

static inline int PyMapping_SetItemString(PyObject *object, const char *key, PyObject *value) {
    return PyPon_Capi()->containers->mapping_set_item_string(object, key, value);
}

#endif /* PON_CAPI_CONTAINERS_INLINE_H */
