#ifndef PON_PYTHON_H
#define PON_PYTHON_H

/* CPython-source compatibility shim for extensions recompiled against Pon.
 *
 * This is NOT CPython's binary ABI. Extensions include this header, compile
 * pon_capi_bootstrap.c and pon_capi_args.c into the module, and the Pon loader
 * injects the process's function tables via PyPon_SetCapi before calling PyInit_*.
 *
 * Dispatch is grouped into per-family tables (PyPonCapiErr, PyPonCapiObject,
 * ...), each declared in its own header under pon_capi/. The top-level
 * PyPonCapi struct only aggregates family-table pointers, so families evolve
 * independently; `size` guards layout drift at load time.
 *
 * Type identity contract: extension code only ever sees FOREIGN PyTypeObject
 * pointers (its own statics, or runtime-owned canonical twins of builtin
 * types reachable through the `types` family). Py_TYPE() is a dispatch call
 * that translates the internal runtime type to its foreign twin; every C-API
 * entry translates foreign type pointers back at the boundary. Never read
 * `ob_type` directly.
 */

#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ---- scalar typedefs consumed by extension headers ---- */

typedef intptr_t Py_ssize_t;
typedef Py_ssize_t Py_hash_t;
typedef size_t Py_uhash_t;
typedef uint32_t Py_UCS4;
typedef uint16_t Py_UCS2;
typedef uint8_t Py_UCS1;
#define PY_SSIZE_T_MAX ((Py_ssize_t)(((size_t)-1) >> 1))
#define PY_SSIZE_T_MIN (-PY_SSIZE_T_MAX - 1)

#define Py_USING_UNICODE 1
#define PY_VERSION "3.14.0"
#define PY_VERSION_HEX 0x030e00f0
#define PY_MAJOR_VERSION 3
#define PY_MINOR_VERSION 14
#define PY_MICRO_VERSION 0
#define PYTHON_API_VERSION 1013

/* ---- object model (mirrors Pon's PyObjectHeader: type word, gc word) ---- */

typedef struct _typeobject PyTypeObject;

typedef struct _object {
    PyTypeObject *ob_type; /* INTERNAL runtime type; use Py_TYPE(), never read */
    uintptr_t gc_meta;
} PyObject;

typedef struct {
    PyObject ob_base;
    Py_ssize_t ob_size;
} PyVarObject;

#define PyObject_HEAD PyObject ob_base;
#define PyObject_VAR_HEAD PyVarObject ob_base;
#define PyObject_HEAD_INIT(type) { (type), 0 },
#define PyVarObject_HEAD_INIT(type, size) { PyObject_HEAD_INIT(type) (size) },

#define Py_SIZE(ob) (((PyVarObject *)(ob))->ob_size)
#define Py_SET_SIZE(ob, size) (((PyVarObject *)(ob))->ob_size = (size))

/* ---- calling conventions ---- */

typedef PyObject *(*PyCFunction)(PyObject *, PyObject *);
typedef PyObject *(*PyCFunctionWithKeywords)(PyObject *, PyObject *, PyObject *);
typedef PyObject *(*vectorcallfunc)(PyObject *, PyObject *const *, size_t, PyObject *);

#define METH_VARARGS 0x0001
#define METH_KEYWORDS 0x0002
#define METH_NOARGS 0x0004
#define METH_O 0x0008
#define METH_CLASS 0x0010
#define METH_STATIC 0x0020
#define METH_COEXIST 0x0040
#define METH_FASTCALL 0x0080
#define METH_METHOD 0x0200

typedef struct PyMethodDef {
    const char *ml_name;
    PyCFunction ml_meth;
    int ml_flags;
    const char *ml_doc;
} PyMethodDef;

/* ---- type-slot function typedefs (CPython names) ---- */

typedef void (*destructor)(PyObject *);
typedef PyObject *(*getattrfunc)(PyObject *, char *);
typedef int (*setattrfunc)(PyObject *, char *, PyObject *);
typedef PyObject *(*getattrofunc)(PyObject *, PyObject *);
typedef int (*setattrofunc)(PyObject *, PyObject *, PyObject *);
typedef PyObject *(*reprfunc)(PyObject *);
typedef Py_hash_t (*hashfunc)(PyObject *);
typedef PyObject *(*richcmpfunc)(PyObject *, PyObject *, int);
typedef PyObject *(*getiterfunc)(PyObject *);
typedef PyObject *(*iternextfunc)(PyObject *);
typedef PyObject *(*ternaryfunc)(PyObject *, PyObject *, PyObject *);
typedef PyObject *(*binaryfunc)(PyObject *, PyObject *);
typedef PyObject *(*unaryfunc)(PyObject *);
typedef int (*inquiry)(PyObject *);
typedef Py_ssize_t (*lenfunc)(PyObject *);
typedef PyObject *(*ssizeargfunc)(PyObject *, Py_ssize_t);
typedef int (*ssizeobjargproc)(PyObject *, Py_ssize_t, PyObject *);
typedef int (*objobjproc)(PyObject *, PyObject *);
typedef int (*objobjargproc)(PyObject *, PyObject *, PyObject *);
typedef int (*visitproc)(PyObject *, void *);
typedef int (*traverseproc)(PyObject *, visitproc, void *);
typedef PyObject *(*allocfunc)(PyTypeObject *, Py_ssize_t);
typedef void (*freefunc)(void *);
typedef PyObject *(*newfunc)(PyTypeObject *, PyObject *, PyObject *);
typedef int (*initproc)(PyObject *, PyObject *, PyObject *);
typedef PyObject *(*descrgetfunc)(PyObject *, PyObject *, PyObject *);
typedef int (*descrsetfunc)(PyObject *, PyObject *, PyObject *);

/* ---- buffer protocol ---- */

typedef struct bufferinfo {
    void *buf;
    PyObject *obj;
    Py_ssize_t len;
    Py_ssize_t itemsize;
    int readonly;
    int ndim;
    char *format;
    Py_ssize_t *shape;
    Py_ssize_t *strides;
    Py_ssize_t *suboffsets;
    void *internal;
} Py_buffer;

typedef int (*getbufferproc)(PyObject *, Py_buffer *, int);
typedef void (*releasebufferproc)(PyObject *, Py_buffer *);

#define PyBUF_SIMPLE 0
#define PyBUF_WRITABLE 0x0001
#define PyBUF_FORMAT 0x0004
#define PyBUF_ND 0x0008
#define PyBUF_STRIDES (0x0010 | PyBUF_ND)
#define PyBUF_C_CONTIGUOUS (0x0020 | PyBUF_STRIDES)
#define PyBUF_F_CONTIGUOUS (0x0040 | PyBUF_STRIDES)
#define PyBUF_ANY_CONTIGUOUS (0x0080 | PyBUF_STRIDES)
#define PyBUF_INDIRECT (0x0100 | PyBUF_STRIDES)
#define PyBUF_CONTIG (PyBUF_ND | PyBUF_WRITABLE)
#define PyBUF_CONTIG_RO (PyBUF_ND)
#define PyBUF_FULL (PyBUF_INDIRECT | PyBUF_WRITABLE | PyBUF_FORMAT)
#define PyBUF_FULL_RO (PyBUF_INDIRECT | PyBUF_FORMAT)
#define PyBUF_RECORDS (PyBUF_STRIDES | PyBUF_WRITABLE | PyBUF_FORMAT)
#define PyBUF_RECORDS_RO (PyBUF_STRIDES | PyBUF_FORMAT)
#define PyBUF_STRIDED (PyBUF_STRIDES | PyBUF_WRITABLE)
#define PyBUF_STRIDED_RO (PyBUF_STRIDES)
#define PyBUF_READ 0x100
#define PyBUF_WRITE 0x200

typedef struct {
    getbufferproc bf_getbuffer;
    releasebufferproc bf_releasebuffer;
} PyBufferProcs;

/* ---- protocol suites referenced from PyTypeObject ---- */

typedef struct {
    binaryfunc nb_add;
    binaryfunc nb_subtract;
    binaryfunc nb_multiply;
    binaryfunc nb_remainder;
    binaryfunc nb_divmod;
    ternaryfunc nb_power;
    unaryfunc nb_negative;
    unaryfunc nb_positive;
    unaryfunc nb_absolute;
    inquiry nb_bool;
    unaryfunc nb_invert;
    binaryfunc nb_lshift;
    binaryfunc nb_rshift;
    binaryfunc nb_and;
    binaryfunc nb_xor;
    binaryfunc nb_or;
    unaryfunc nb_int;
    void *nb_reserved;
    unaryfunc nb_float;
    binaryfunc nb_inplace_add;
    binaryfunc nb_inplace_subtract;
    binaryfunc nb_inplace_multiply;
    binaryfunc nb_inplace_remainder;
    ternaryfunc nb_inplace_power;
    binaryfunc nb_inplace_lshift;
    binaryfunc nb_inplace_rshift;
    binaryfunc nb_inplace_and;
    binaryfunc nb_inplace_xor;
    binaryfunc nb_inplace_or;
    binaryfunc nb_floor_divide;
    binaryfunc nb_true_divide;
    binaryfunc nb_inplace_floor_divide;
    binaryfunc nb_inplace_true_divide;
    unaryfunc nb_index;
    binaryfunc nb_matrix_multiply;
    binaryfunc nb_inplace_matrix_multiply;
} PyNumberMethods;

typedef struct {
    lenfunc sq_length;
    binaryfunc sq_concat;
    ssizeargfunc sq_repeat;
    ssizeargfunc sq_item;
    void *was_sq_slice;
    ssizeobjargproc sq_ass_item;
    void *was_sq_ass_slice;
    objobjproc sq_contains;
    binaryfunc sq_inplace_concat;
    ssizeargfunc sq_inplace_repeat;
} PySequenceMethods;

typedef struct {
    lenfunc mp_length;
    binaryfunc mp_subscript;
    objobjargproc mp_ass_subscript;
} PyMappingMethods;

typedef struct {
    unaryfunc am_await;
    unaryfunc am_aiter;
    unaryfunc am_anext;
    void *am_send;
} PyAsyncMethods;

/* ---- member/getset descriptors (see structmember.h for T_* codes) ---- */

typedef struct PyMemberDef {
    const char *name;
    int type;
    Py_ssize_t offset;
    int flags;
    const char *doc;
} PyMemberDef;

typedef PyObject *(*getter)(PyObject *, void *);
typedef int (*setter)(PyObject *, PyObject *, void *);

typedef struct PyGetSetDef {
    const char *name;
    getter get;
    setter set;
    const char *doc;
    void *closure;
} PyGetSetDef;

/* ---- FOREIGN PyTypeObject ----
 * Extension-facing static type storage, CPython 3.x member list. This struct
 * is never the runtime's internal type representation: PyType_Ready()
 * translates it into a native Pon type and registers the twin mapping.
 * `tp_pon_twin` is reserved for that mapping; static initializers leave it 0.
 */
struct _typeobject {
    PyVarObject ob_base;
    const char *tp_name;
    Py_ssize_t tp_basicsize;
    Py_ssize_t tp_itemsize;

    destructor tp_dealloc;
    Py_ssize_t tp_vectorcall_offset;
    getattrfunc tp_getattr;
    setattrfunc tp_setattr;
    PyAsyncMethods *tp_as_async;
    reprfunc tp_repr;

    PyNumberMethods *tp_as_number;
    PySequenceMethods *tp_as_sequence;
    PyMappingMethods *tp_as_mapping;

    hashfunc tp_hash;
    ternaryfunc tp_call;
    reprfunc tp_str;
    getattrofunc tp_getattro;
    setattrofunc tp_setattro;

    PyBufferProcs *tp_as_buffer;

    unsigned long tp_flags;
    const char *tp_doc;
    traverseproc tp_traverse;
    inquiry tp_clear;
    richcmpfunc tp_richcompare;
    Py_ssize_t tp_weaklistoffset;
    getiterfunc tp_iter;
    iternextfunc tp_iternext;

    PyMethodDef *tp_methods;
    PyMemberDef *tp_members;
    PyGetSetDef *tp_getset;
    PyTypeObject *tp_base;
    PyObject *tp_dict;
    descrgetfunc tp_descr_get;
    descrsetfunc tp_descr_set;
    Py_ssize_t tp_dictoffset;
    initproc tp_init;
    allocfunc tp_alloc;
    newfunc tp_new;
    freefunc tp_free;
    inquiry tp_is_gc;
    PyObject *tp_bases;
    PyObject *tp_mro;
    PyObject *tp_cache;
    void *tp_subclasses;
    PyObject *tp_weaklist;
    destructor tp_del;
    unsigned int tp_version_tag;
    destructor tp_finalize;
    vectorcallfunc tp_vectorcall;
    unsigned char tp_watched;
    uint16_t tp_versions_used;

    /* Pon: native twin pointer, filled by PyType_Ready(). Reserved. */
    void *tp_pon_twin;
};

/* type flags consumed by extension initializers */
#define Py_TPFLAGS_DEFAULT (0)
#define Py_TPFLAGS_BASETYPE (1UL << 10)
#define Py_TPFLAGS_HAVE_GC (1UL << 14)
#define Py_TPFLAGS_HEAPTYPE (1UL << 9)
#define Py_TPFLAGS_HAVE_VECTORCALL (1UL << 11)
#define Py_TPFLAGS_IMMUTABLETYPE (1UL << 8)
#define Py_TPFLAGS_DISALLOW_INSTANTIATION (1UL << 7)
#define Py_TPFLAGS_LONG_SUBCLASS (1UL << 24)
#define Py_TPFLAGS_LIST_SUBCLASS (1UL << 25)
#define Py_TPFLAGS_TUPLE_SUBCLASS (1UL << 26)
#define Py_TPFLAGS_BYTES_SUBCLASS (1UL << 27)
#define Py_TPFLAGS_UNICODE_SUBCLASS (1UL << 28)
#define Py_TPFLAGS_DICT_SUBCLASS (1UL << 29)
#define Py_TPFLAGS_BASE_EXC_SUBCLASS (1UL << 30)
#define Py_TPFLAGS_TYPE_SUBCLASS (1UL << 31)

/* rich-comparison opcodes */
#define Py_LT 0
#define Py_LE 1
#define Py_EQ 2
#define Py_NE 3
#define Py_GT 4
#define Py_GE 5

/* ---- module definitions ---- */

typedef struct PyModuleDef_Base {
    PyObject ob_base;
    void *m_init;
    Py_ssize_t m_index;
    PyObject *m_copy;
} PyModuleDef_Base;

typedef struct PyModuleDef_Slot {
    int slot;
    void *value;
} PyModuleDef_Slot;

#define Py_mod_create 1
#define Py_mod_exec 2
#define Py_mod_multiple_interpreters 3
#define Py_mod_gil 4

#define Py_MOD_GIL_USED ((void *)0)
#define Py_MOD_GIL_NOT_USED ((void *)1)
#define Py_MOD_MULTIPLE_INTERPRETERS_NOT_SUPPORTED ((void *)0)

typedef struct PyModuleDef {
    PyModuleDef_Base m_base;
    const char *m_name;
    const char *m_doc;
    Py_ssize_t m_size;
    PyMethodDef *m_methods;
    PyModuleDef_Slot *m_slots;
    traverseproc m_traverse;
    inquiry m_clear;
    freefunc m_free;
} PyModuleDef;

#define PyModuleDef_HEAD_INIT { PyObject_HEAD_INIT(NULL) NULL, 0, NULL }

#ifndef PyMODINIT_FUNC
#define PyMODINIT_FUNC PyObject *
#endif

/* ---- family tables ---- */

#include "pon_capi/core.h"
#include "pon_capi/err.h"
#include "pon_capi/numbers.h"
#include "pon_capi/strings.h"
#include "pon_capi/containers.h"
#include "pon_capi/runtime.h"
#include "pon_capi/object.h"
#include "pon_capi/typeobj.h"

#include "pon_capi/args.h"
typedef struct PyPonCapi {
    /* sizeof(PyPonCapi) as built by the runtime; bootstrap rejects drift. */
    size_t size;
    const PyPonCapiCore *core;
    const PyPonCapiErr *err;
    const PyPonCapiNumbers *numbers;
    const PyPonCapiStrings *strings;
    const PyPonCapiContainers *containers;
    const PyPonCapiRuntime *runtime_;
    const PyPonCapiObject *object_;
    const PyPonCapiTypeObj *typeobj;
    /* Family expansion point: append pointer fields only; never reorder. */
} PyPonCapi;

int PyPon_SetCapi(const PyPonCapi *api);
const PyPonCapi *PyPon_GetCapi(void);

static inline const PyPonCapi *PyPon_Capi(void) {
    return PyPon_GetCapi();
}

#include "pon_capi/core_inline.h"
#include "pon_capi/numbers_inline.h"
#include "pon_capi/strings_inline.h"
#include "pon_capi/containers_inline.h"
#include "pon_capi/runtime_inline.h"
#include "pon_capi/object_inline.h"
#include "pon_capi/typeobj_inline.h"

#ifdef __cplusplus
}
#endif

#endif /* PON_PYTHON_H */
