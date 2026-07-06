#include <Python.h>

/* Injected function-table hub; set once per extension by the Pon loader. */
static const PyPonCapi *pypon_capi;

/* Bootstrap-local builtin twin globals: REAL storage so `&PyLong_Type` is a
 * link-time constant. PyPon_SetCapi hands them to the runtime, which fills
 * their descriptive fields and maps their addresses at every boundary. */
PyTypeObject PyType_Type;
PyTypeObject PyBaseObject_Type;
PyTypeObject PyLong_Type;
PyTypeObject PyBool_Type;
PyTypeObject PyFloat_Type;
PyTypeObject PyComplex_Type;
PyTypeObject PyUnicode_Type;
PyTypeObject PyBytes_Type;
PyTypeObject PyByteArray_Type;
PyTypeObject PyTuple_Type;
PyTypeObject PyList_Type;
PyTypeObject PyDict_Type;
PyTypeObject PySet_Type;
PyTypeObject PyFrozenSet_Type;
PyTypeObject PySlice_Type;
PyTypeObject PyMemoryView_Type;
PyTypeObject PyCapsule_Type;
PyTypeObject _PyNone_Type;
PyTypeObject PyRange_Type;

PyTypeObject *const _PyPon_LocalTwins[PON_BUILTIN_TYPE_COUNT] = {
    [PON_TID_TYPE] = &PyType_Type,
    [PON_TID_OBJECT] = &PyBaseObject_Type,
    [PON_TID_LONG] = &PyLong_Type,
    [PON_TID_BOOL] = &PyBool_Type,
    [PON_TID_FLOAT] = &PyFloat_Type,
    [PON_TID_COMPLEX] = &PyComplex_Type,
    [PON_TID_UNICODE] = &PyUnicode_Type,
    [PON_TID_BYTES] = &PyBytes_Type,
    [PON_TID_BYTEARRAY] = &PyByteArray_Type,
    [PON_TID_TUPLE] = &PyTuple_Type,
    [PON_TID_LIST] = &PyList_Type,
    [PON_TID_DICT] = &PyDict_Type,
    [PON_TID_SET] = &PySet_Type,
    [PON_TID_FROZENSET] = &PyFrozenSet_Type,
    [PON_TID_SLICE] = &PySlice_Type,
    [PON_TID_MEMORYVIEW] = &PyMemoryView_Type,
    [PON_TID_CAPSULE] = &PyCapsule_Type,
    [PON_TID_NONE_TYPE] = &_PyNone_Type,
    [PON_TID_RANGE] = &PyRange_Type,
};

int PyPon_SetCapi(const PyPonCapi *api) {
    if (api == 0 || api->size != sizeof(PyPonCapi)) {
        return -1;
    }
    if (api->core->register_local_twins(_PyPon_LocalTwins, PON_BUILTIN_TYPE_COUNT) != 0) {
        return -1;
    }
    pypon_capi = api;
    return 0;
}
const PyPonCapi *PyPon_GetCapi(void) {
    return pypon_capi;
}
