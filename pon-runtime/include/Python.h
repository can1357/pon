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

#include <assert.h>
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

/* Extension code (pythoncapi-compat) spells declarations through these;
 * recompiled extensions have no DLL surface, so they are identity macros. */
#define PyAPI_FUNC(RTYPE) RTYPE
#define PyAPI_DATA(RTYPE) extern RTYPE
#if defined(__GNUC__) || defined(__clang__)
#  define Py_GCC_ATTRIBUTE(x) __attribute__(x)
#else
#  define Py_GCC_ATTRIBUTE(x)
#endif
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

/* ---- CPython 3.14 str object layout (compile surface ONLY) ----
 * numpy embeds PyUnicodeObject as the base of its unicode scalar struct
 * (arrayscalars.h) and pythoncapi-compat's PyUnstable_Unicode_GET_CACHED_HASH
 * (which numpy never calls) reads PyASCIIObject.hash. These mirror CPython
 * 3.14's field layout for sizeof/offsetof purposes. Pon str objects do NOT
 * use this layout: reading these fields from a live str yields garbage.
 * Use PyUnicode_DATA/KIND/GET_LENGTH/READ (table-backed) for real access. */
typedef struct {
    PyObject ob_base;
    Py_ssize_t length;
    Py_hash_t hash;
    struct {
        unsigned int interned:2;
        unsigned int kind:3;
        unsigned int compact:1;
        unsigned int ascii:1;
        unsigned int statically_allocated:1;
        unsigned int :24;
    } state;
} PyASCIIObject;

typedef struct {
    PyASCIIObject _base;
    Py_ssize_t utf8_length;
    char *utf8;
} PyCompactUnicodeObject;

typedef struct {
    PyCompactUnicodeObject _base;
    union {
        void *any;
        Py_UCS1 *latin1;
        Py_UCS2 *ucs2;
        Py_UCS4 *ucs4;
    } data;
} PyUnicodeObject;

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

/* ---- multi-phase module initialization (CPython 3.14) ----
 * Body is in pon_capi/runtime_inline.h, after PyPonCapi is declared.
 */
static inline PyObject *PyModuleDef_Init(PyModuleDef *def);

#ifndef PyMODINIT_FUNC
#define PyMODINIT_FUNC PyObject *
#endif

/* ---- structural runtime compatibility (NumPy C-API surface) ----
 *
 * This block is intentionally local and contiguous: it supplies small CPython
 * structural helpers that do not belong to a dispatch family, while leaving
 * real behavior (thread state, frames, contextvars, builtins) in runtime.h.
 */
typedef intptr_t Py_intptr_t;
typedef uintptr_t Py_uintptr_t;

typedef struct _frame PyFrameObject;
typedef struct PyCodeObject PyCodeObject;

typedef struct {
    uint8_t _bits;
} PyMutex;

/* Single-interpreter Pon rarely contends here; this is still a correct C11
 * acquire/release spin lock for extension code that keeps CPython's mutex
 * bracketing.
 */
static inline void PyMutex_Lock(PyMutex *mutex) {
    while (__atomic_exchange_n(&mutex->_bits, (uint8_t)1, __ATOMIC_ACQUIRE) != 0) {
    }
}

static inline void PyMutex_Unlock(PyMutex *mutex) {
    __atomic_store_n(&mutex->_bits, (uint8_t)0, __ATOMIC_RELEASE);
}

/* vectorcallfunc itself is declared with the calling-convention typedefs above
 * because PyTypeObject embeds it; these are the matching flag helpers.
 */
#define PY_VECTORCALL_ARGUMENTS_OFFSET ((size_t)1 << (8 * sizeof(size_t) - 1))

static inline Py_ssize_t PyVectorcall_NARGS(size_t n) {
    return (Py_ssize_t)(n & ~PY_VECTORCALL_ARGUMENTS_OFFSET);
}

static inline void Py_SET_TYPE(PyObject *ob, PyTypeObject *type) {
    ob->ob_type = type;
}

/* All Pon objects are GC-managed; CPython-style immortality has no meaning. */
static inline void _Py_SetImmortal(PyObject *op) {
    (void)op;
}

/* Conservative answer: refcounts do not exist, and callers use this only as an
 * optimization hint.
 */
static inline int PyUnstable_Object_IsUniquelyReferenced(PyObject *op) {
    (void)op;
    return 0;
}

/* ---- PyType_FromSpec heap-type compatibility (CPython 3.14) ---- */

typedef struct {
    int slot;
    void *pfunc;
} PyType_Slot;

typedef struct {
    const char *name;
    int basicsize;
    int itemsize;
    unsigned int flags;
    PyType_Slot *slots;
} PyType_Spec;

/* Stable-ABI type slot ids: keep in exact sync with CPython 3.14 typeslots.h. */
#define Py_bf_getbuffer 1
#define Py_bf_releasebuffer 2
#define Py_mp_ass_subscript 3
#define Py_mp_length 4
#define Py_mp_subscript 5
#define Py_nb_absolute 6
#define Py_nb_add 7
#define Py_nb_and 8
#define Py_nb_bool 9
#define Py_nb_divmod 10
#define Py_nb_float 11
#define Py_nb_floor_divide 12
#define Py_nb_index 13
#define Py_nb_inplace_add 14
#define Py_nb_inplace_and 15
#define Py_nb_inplace_floor_divide 16
#define Py_nb_inplace_lshift 17
#define Py_nb_inplace_multiply 18
#define Py_nb_inplace_or 19
#define Py_nb_inplace_power 20
#define Py_nb_inplace_remainder 21
#define Py_nb_inplace_rshift 22
#define Py_nb_inplace_subtract 23
#define Py_nb_inplace_true_divide 24
#define Py_nb_inplace_xor 25
#define Py_nb_int 26
#define Py_nb_invert 27
#define Py_nb_lshift 28
#define Py_nb_multiply 29
#define Py_nb_negative 30
#define Py_nb_or 31
#define Py_nb_positive 32
#define Py_nb_power 33
#define Py_nb_remainder 34
#define Py_nb_rshift 35
#define Py_nb_subtract 36
#define Py_nb_true_divide 37
#define Py_nb_xor 38
#define Py_sq_ass_item 39
#define Py_sq_concat 40
#define Py_sq_contains 41
#define Py_sq_inplace_concat 42
#define Py_sq_inplace_repeat 43
#define Py_sq_item 44
#define Py_sq_length 45
#define Py_sq_repeat 46
#define Py_tp_alloc 47
#define Py_tp_base 48
#define Py_tp_bases 49
#define Py_tp_call 50
#define Py_tp_clear 51
#define Py_tp_dealloc 52
#define Py_tp_del 53
#define Py_tp_descr_get 54
#define Py_tp_descr_set 55
#define Py_tp_doc 56
#define Py_tp_getattr 57
#define Py_tp_getattro 58
#define Py_tp_hash 59
#define Py_tp_init 60
#define Py_tp_is_gc 61
#define Py_tp_iter 62
#define Py_tp_iternext 63
#define Py_tp_methods 64
#define Py_tp_new 65
#define Py_tp_repr 66
#define Py_tp_richcompare 67
#define Py_tp_setattr 68
#define Py_tp_setattro 69
#define Py_tp_str 70
#define Py_tp_traverse 71
#define Py_tp_members 72
#define Py_tp_getset 73
#define Py_tp_free 74
#define Py_nb_matrix_multiply 75
#define Py_nb_inplace_matrix_multiply 76
#define Py_am_await 77
#define Py_am_aiter 78
#define Py_am_anext 79
#define Py_tp_finalize 80
#define Py_am_send 81
#define Py_tp_vectorcall 82
#define Py_tp_token 83

struct _dictkeysobject;

struct _specialization_cache {
    PyObject *getitem;
    uint32_t getitem_version;
    PyObject *init;
};

typedef struct _heaptypeobject {
    PyTypeObject ht_type;
    PyAsyncMethods as_async;
    PyNumberMethods as_number;
    PyMappingMethods as_mapping;
    PySequenceMethods as_sequence;
    PyBufferProcs as_buffer;
    PyObject *ht_name;
    PyObject *ht_slots;
    PyObject *ht_qualname;
    struct _dictkeysobject *ht_cached_keys;
    PyObject *ht_module;
    char *_ht_tpname;
    void *ht_token;
    struct _specialization_cache _spec_cache;
#ifdef Py_GIL_DISABLED
    Py_ssize_t unique_id;
#endif
} PyHeapTypeObject;

static inline PyObject *PyType_FromSpec(PyType_Spec *spec);
static inline PyObject *PyType_FromSpecWithBases(PyType_Spec *spec, PyObject *bases);
static inline PyObject *PyType_FromModuleAndSpec(PyObject *module, PyType_Spec *spec, PyObject *bases);

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
