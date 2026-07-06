#ifndef PON_PYTHON_H
#define PON_PYTHON_H

/* CPython's include-guard sentinel: cython-generated C (and other
 * refcount-portable extensions) gate compilation on it being defined. */
#define Py_PYTHON_H

/* Cython-generated modules embed BOTH compressed and plain string tables and
 * pick at C-compile time: force the plain branch so module init never needs
 * PyMemoryView_FromMemory + zlib before the runtime is fully up. */
#define CYTHON_COMPRESS_STRINGS 0

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
#include <ctype.h>
#include <errno.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <wctype.h>
#include "compile.h"

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

/* ---- CPython-compatible complex scalar value ----
 * Numpy expects `Py_complex` to be a by-value struct whose fields are readable
 * as `.real` and `.imag`, matching CPython's public C API. Pon complex objects
 * still use Pon's own heap layout; this struct is only the C-API transport type.
 */
typedef struct {
    double real;
    double imag;
} Py_complex;

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

static const unsigned long Py_Version = PY_VERSION_HEX;

static inline void Py_FatalError(const char *message) {
    fputs("Fatal Python error: ", stderr);
    fputs(message != NULL ? message : "(null)", stderr);
    fputc('\n', stderr);
    abort();
}

/* Pon's C-API shim is source-compatible, not CPython-internal-layout
 * compatible.  Force Cython away from direct thread-state/list/long/dict
 * internals while leaving the public CPython API branch selected.
 */
#ifndef CYTHON_FAST_THREAD_STATE
#define CYTHON_FAST_THREAD_STATE 0
#endif
#ifndef CYTHON_ASSUME_SAFE_MACROS
#define CYTHON_ASSUME_SAFE_MACROS 0
#endif
#ifndef CYTHON_USE_PYLIST_INTERNALS
#define CYTHON_USE_PYLIST_INTERNALS 0
#endif
#ifndef CYTHON_USE_PYLONG_INTERNALS
#define CYTHON_USE_PYLONG_INTERNALS 0
#endif
#ifndef CYTHON_USE_DICT_VERSIONS
#define CYTHON_USE_DICT_VERSIONS 0
#endif
#ifndef CYTHON_USE_UNICODE_INTERNALS
#define CYTHON_USE_UNICODE_INTERNALS 0
#endif

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
typedef struct {
    PyObject ob_base;
    Py_complex cval;
} PyComplexObject;
#define PyVarObject_HEAD_INIT(type, size) { PyObject_HEAD_INIT(type) (size) },

#define Py_SIZE(ob) (((PyVarObject *)(ob))->ob_size)
#define Py_SET_SIZE(ob, size) (((PyVarObject *)(ob))->ob_size = (size))

typedef struct PyGenObject {
    PyObject ob_base;
} PyGenObject;

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
typedef PyObject *(*PyCFunctionFast)(PyObject *, PyObject *const *, Py_ssize_t);
typedef PyObject *(*PyCFunctionFastWithKeywords)(PyObject *, PyObject *const *, Py_ssize_t, PyObject *);
typedef PyCFunctionFast _PyCFunctionFast;
typedef PyCFunctionFastWithKeywords _PyCFunctionFastWithKeywords;

typedef enum {
    PYGEN_RETURN = 0,
    PYGEN_ERROR = -1,
    PYGEN_NEXT = 1,
} PySendResult;

typedef PySendResult (*sendfunc)(PyObject *, PyObject *, PyObject **);

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

typedef struct {
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

#define PyBUF_MAX_NDIM 64
#define PyBUF_SIMPLE 0
#define PyBUF_WRITABLE 0x0001
#define PyBUF_WRITEABLE PyBUF_WRITABLE
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
    sendfunc am_send;
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
#define _Py_TPFLAGS_HAVE_VECTORCALL Py_TPFLAGS_HAVE_VECTORCALL
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
/* Module init entries must stay dlsym-visible even under
 * -fvisibility=hidden (numpy compiles extensions that way). */
#ifdef __cplusplus
#define PyMODINIT_FUNC extern "C" __attribute__((visibility("default"))) PyObject *
#else
#define PyMODINIT_FUNC __attribute__((visibility("default"))) PyObject *
#endif
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
#ifndef PON_CODEOBJECT_STRUCT_DEFINED
#define PON_CODEOBJECT_STRUCT_DEFINED 1
typedef struct PyCodeObject {
    PyObject_HEAD
    int _co_firsttraceable;
    int co_firstlineno;
    PyObject *co_filename;
    PyObject *co_name;
    PyObject *co_qualname;
    int co_nfreevars;
} PyCodeObject;
#endif

typedef struct _traceback {
    PyObject ob_base;
    struct _traceback *tb_next;
    PyFrameObject *tb_frame;
    int tb_lasti;
    int tb_lineno;
} PyTracebackObject;

_Static_assert(offsetof(PyTracebackObject, tb_next) == sizeof(PyObject),
               "Pon PyTracebackObject tb_next must mirror traceback prefix");
_Static_assert(offsetof(PyTracebackObject, tb_frame) == sizeof(PyObject) + sizeof(PyObject *),
               "Pon PyTracebackObject tb_frame must mirror traceback frame slot");

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

#define Py_SET_TYPE(ob, type) ((void)(((PyObject *)(ob))->ob_type = (PyTypeObject *)(type)))

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

/* Pon tuple layout mirrors the real runtime tuple carrier: header, length,
 * then an out-of-line item pointer vector. This keeps PyTuple_GET_ITEM an
 * lvalue (`&PyTuple_GET_ITEM(t, 0)`) without fabricating CPython's inline
 * ob_item[] tail, which Pon does not have.
 */
#ifndef PON_HAVE_PYTUPLEOBJECT_LAYOUT
#define PON_HAVE_PYTUPLEOBJECT_LAYOUT 1

typedef struct {
    PyObject ob_base;
    Py_ssize_t ob_size;
    Py_ssize_t allocated;
    PyObject **ob_item;
} PyListObject;
typedef struct {
    PyObject ob_base;
    Py_ssize_t len;
    union {
        PyObject **items;
        PyObject **ob_item;
    };
} PyTupleObject;
#endif

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

/* ---- CPython header-compat compile surface (NumPy 2.5) ----
 * These are source-compatibility helpers whose behavior either maps to an
 * existing Pon C-API table entry above or is a compile-time constant/no-op for
 * Pon's single-interpreter runtime. Keep real runtime behavior in the family
 * tables; this block is only for CPython header macros and tiny inlines.
 */

#ifndef Py_MIN
#define Py_MIN(x, y) (((x) > (y)) ? (y) : (x))
#endif
#ifndef Py_MAX
#define Py_MAX(x, y) (((x) > (y)) ? (x) : (y))
#endif

#ifndef SIZEOF_VOID_P
#  if defined(__SIZEOF_POINTER__)
#    define SIZEOF_VOID_P __SIZEOF_POINTER__
#  elif defined(_WIN64) || defined(__LP64__) || defined(_LP64)
#    define SIZEOF_VOID_P 8
#  else
#    define SIZEOF_VOID_P 4
#  endif
#endif
#ifndef SIZEOF_SIZE_T
#  if defined(__SIZEOF_SIZE_T__)
#    define SIZEOF_SIZE_T __SIZEOF_SIZE_T__
#  else
#    define SIZEOF_SIZE_T SIZEOF_VOID_P
#  endif
#endif
#ifndef SIZEOF_LONG
#  if defined(__SIZEOF_LONG__)
#    define SIZEOF_LONG __SIZEOF_LONG__
#  elif defined(_WIN64)
#    define SIZEOF_LONG 4
#  elif defined(__LP64__) || defined(_LP64)
#    define SIZEOF_LONG 8
#  else
#    define SIZEOF_LONG 4
#  endif
#endif
#ifndef SIZEOF_INT
#  if defined(__SIZEOF_INT__)
#    define SIZEOF_INT __SIZEOF_INT__
#  else
#    define SIZEOF_INT 4
#  endif
#endif
#ifndef SIZEOF_LONG_LONG
#  if defined(__SIZEOF_LONG_LONG__)
#    define SIZEOF_LONG_LONG __SIZEOF_LONG_LONG__
#  else
#    define SIZEOF_LONG_LONG 8
#  endif
#endif

#ifndef Py_CHARMASK
#define Py_CHARMASK(c) ((unsigned char)((c) & 0xff))
#endif

/* CPython uses Unicode database tables. Pon's compile shim delegates to the C
 * library's wide-character predicates: this is locale-sensitive, but matches
 * the BMP whitespace NumPy parses on this target. Spot-check:
 * python3.14 str.isspace vs Darwin iswspace agreed for U+0009, U+0020,
 * U+00A0, U+1680, U+2000, U+2028, U+2029, and U+3000.
 */
#define Py_UNICODE_ISSPACE(ch) (iswspace((wint_t)(Py_UCS4)(ch)) != 0)
#define Py_UNICODE_ISDIGIT(ch) (iswdigit((wint_t)(Py_UCS4)(ch)) != 0)
#define Py_UNICODE_ISALPHA(ch) (iswalpha((wint_t)(Py_UCS4)(ch)) != 0)
#define Py_UNICODE_ISALNUM(ch) (iswalnum((wint_t)(Py_UCS4)(ch)) != 0)
#define Py_UNICODE_ISLOWER(ch) (iswlower((wint_t)(Py_UCS4)(ch)) != 0)
#define Py_UNICODE_ISUPPER(ch) (iswupper((wint_t)(Py_UCS4)(ch)) != 0)
#define Py_UNICODE_TOLOWER(ch) ((Py_UCS4)towlower((wint_t)(Py_UCS4)(ch)))
#define Py_UNICODE_TOUPPER(ch) ((Py_UCS4)towupper((wint_t)(Py_UCS4)(ch)))
/* Approximations over wctype (documented divergence from CPython's own
 * category tables, exact for ASCII and common BMP ranges): ISDECIMAL and
 * ISNUMERIC collapse onto digit-ness (Nd); ISTITLE onto uppercase (the Lt
 * category has no wctype probe; titlecase digraphs are vanishingly rare). */
#define Py_UNICODE_ISDECIMAL(ch) (iswdigit((wint_t)(Py_UCS4)(ch)) != 0)
#define Py_UNICODE_ISNUMERIC(ch) (iswdigit((wint_t)(Py_UCS4)(ch)) != 0)
#define Py_UNICODE_ISTITLE(ch) (iswupper((wint_t)(Py_UCS4)(ch)) != 0)

/* ---- CPython pyport/pyconfig compat tail ---- */
#define PY_LONG_LONG long long
#define PY_INT64_T int64_t
#define PY_UINT64_T uint64_t
typedef uint32_t digit;
#define PyLong_SHIFT 30
#define PyLong_MASK ((1U << PyLong_SHIFT) - 1)
/* Release-mode CPython Py_SAFE_DOWNCAST: plain cast, no assertion. */
#define Py_SAFE_DOWNCAST(VALUE, WIDE, NARROW) ((NARROW)(VALUE))

/* Recursion depth is guarded by Pon's own call machinery; the C-level
 * bookkeeping is a no-op that always grants entry. */
static inline int Py_EnterRecursiveCall(const char *where) { (void)where; return 0; }
static inline void Py_LeaveRecursiveCall(void) {}

#define PyExceptionInstance_Class(x) ((PyObject *)Py_TYPE(x))

static inline int PyExceptionInstance_Check(PyObject *object) {
    return object != NULL && PyObject_IsInstance(object, PyExc_BaseException);
}

static inline int PyExceptionClass_Check(PyObject *object) {
    return object != NULL && PyObject_IsSubclass(object, PyExc_BaseException);
}

static inline PyObject *PyException_GetTraceback(PyObject *object) {
    return PyObject_GetAttrString(object, "__traceback__");
}

static inline void PyErr_GetExcInfo(PyObject **ptype, PyObject **pvalue, PyObject **ptraceback) {
    if (ptype != NULL) {
        *ptype = NULL;
    }
    if (pvalue != NULL) {
        *pvalue = NULL;
    }
    if (ptraceback != NULL) {
        *ptraceback = NULL;
    }
}

static inline void PyErr_SetExcInfo(PyObject *type, PyObject *value, PyObject *traceback) {
    Py_XDECREF(type);
    Py_XDECREF(value);
    Py_XDECREF(traceback);
}

static inline PyObject *PyErr_GetRaisedException(void) {
    PyObject *type = NULL;
    PyObject *value = NULL;
    PyObject *traceback = NULL;
    PyErr_Fetch(&type, &value, &traceback);
    Py_XDECREF(type);
    Py_XDECREF(traceback);
    return value;
}

static inline void PyErr_SetHandledException(PyObject *exception) {
    (void)exception;
}
/* tracemalloc is not modeled under Pon: tracking no-ops report success. */
#define PYMEM_DOMAIN_RAW 0
#define PYMEM_DOMAIN_MEM 1
#define PYMEM_DOMAIN_OBJ 2
static inline int PyTraceMalloc_Track(unsigned int domain, uintptr_t ptr, size_t size) {
    (void)domain; (void)ptr; (void)size;
    return 0;
}
static inline int PyTraceMalloc_Untrack(unsigned int domain, uintptr_t ptr) {
    (void)domain; (void)ptr;
    return 0;
}

/* Single interpreter, no GIL: the calling thread always "holds" it. */
static inline int PyGILState_Check(void) { return 1; }

/* CPython 3.14 bytes layout (compile surface ONLY — same caveat as
 * PyUnicodeObject: Pon bytes do NOT use this layout; reading these fields
 * from a live bytes object yields garbage). numpy sizes its bytes-scalar
 * base struct from it. */
typedef struct {
    PyVarObject ob_base;
    Py_hash_t ob_shash;
    char ob_sval[1];
} PyBytesObject;

/* Pon has one interpreter and no free-threaded object locks; preserve CPython's
 * bracketing syntax without taking locks.
 */
#define Py_BEGIN_CRITICAL_SECTION(op) {
#define Py_END_CRITICAL_SECTION() }
#define Py_BEGIN_CRITICAL_SECTION2(a, b) {
#define Py_END_CRITICAL_SECTION2() }

#define Py_VISIT(op)                                                    \
    do {                                                                \
        if (op) {                                                       \
            int vret = visit((PyObject *)(op), arg);                    \
            if (vret) {                                                 \
                return vret;                                            \
            }                                                           \
        }                                                               \
    } while (0)

#define Py_RETURN_RICHCOMPARE(val1, val2, op)                           \
    do {                                                                \
        switch (op) {                                                   \
        case Py_EQ: if ((val1) == (val2)) Py_RETURN_TRUE; Py_RETURN_FALSE; \
        case Py_NE: if ((val1) != (val2)) Py_RETURN_TRUE; Py_RETURN_FALSE; \
        case Py_LT: if ((val1) < (val2)) Py_RETURN_TRUE; Py_RETURN_FALSE; \
        case Py_GT: if ((val1) > (val2)) Py_RETURN_TRUE; Py_RETURN_FALSE; \
        case Py_LE: if ((val1) <= (val2)) Py_RETURN_TRUE; Py_RETURN_FALSE; \
        case Py_GE: if ((val1) >= (val2)) Py_RETURN_TRUE; Py_RETURN_FALSE; \
        default:                                                        \
            PyErr_BadInternalCall();                                    \
            return NULL;                                                \
        }                                                               \
    } while (0)

#ifndef Py_CLEANUP_SUPPORTED
#define Py_CLEANUP_SUPPORTED 0x20000
#endif
#ifndef Py_TPFLAGS_SEQUENCE
#define Py_TPFLAGS_SEQUENCE (1UL << 5)
#endif
#ifndef Py_TPFLAGS_MAPPING
#define Py_TPFLAGS_MAPPING (1UL << 6)
#endif
#ifndef Py_TPFLAGS_METHOD_DESCRIPTOR
#define Py_TPFLAGS_METHOD_DESCRIPTOR (1UL << 17)
#endif
#ifndef _Py_TPFLAGS_HAVE_VECTORCALL
#define _Py_TPFLAGS_HAVE_VECTORCALL Py_TPFLAGS_HAVE_VECTORCALL
#endif

#ifndef Py_TPFLAGS_HAVE_VERSION_TAG
#define Py_TPFLAGS_HAVE_VERSION_TAG 0
#endif

#define PyDoc_STR(str) str
#define PyDoc_VAR(name) static const char name[]
#define PyDoc_STRVAR(name, str) PyDoc_VAR(name) = PyDoc_STR(str)


static inline int PyObject_GC_IsFinalized(PyObject *object) {
    (void)object;
    return 0;
}

static inline int PyObject_CallFinalizerFromDealloc(PyObject *object) {
    (void)object;
    return 0;
}

static inline int PyType_IS_GC(PyTypeObject *type) {
    return type != NULL && (type->tp_flags & Py_TPFLAGS_HAVE_GC) != 0;
}

static inline void PyUnstable_Object_EnableDeferredRefcount(PyObject *object) {
    (void)object;
}

static inline int64_t PyInterpreterState_GetID(PyInterpreterState *interp) {
    (void)interp;
    return 0;
}
/* Pon pins objects via Py_INCREF/Py_DECREF, not an in-object refcount field. */
#define Py_SET_REFCNT(op, n) do { (void)(op); (void)(n); } while (0)

#define PyObject_INIT(op, typeobj) PyObject_Init((PyObject *)(op), (typeobj))
#define PyObject_INIT_VAR(op, typeobj, size) PyObject_InitVar((PyVarObject *)(op), (typeobj), (size))
#define PyObject_FREE PyObject_Free
#ifndef PyMem_MALLOC
#define PyMem_MALLOC(size) PyMem_Malloc(size)
#endif

static inline PyVarObject *PyObject_InitVar(PyVarObject *op, PyTypeObject *type, Py_ssize_t size) {
    if (PyObject_Init((PyObject *)op, type) == NULL) {
        return NULL;
    }
    Py_SET_SIZE(op, size);
    return op;
}

#if ((SIZEOF_VOID_P - 1) & SIZEOF_VOID_P) != 0
#  error "_PyObject_VAR_SIZE requires SIZEOF_VOID_P be a power of 2"
#endif
static inline size_t _PyObject_VAR_SIZE(PyTypeObject *type, Py_ssize_t nitems) {
    size_t size = (size_t)type->tp_basicsize;
    size += (size_t)nitems * (size_t)type->tp_itemsize;
    return (size + (size_t)(SIZEOF_VOID_P - 1)) & ~(size_t)(SIZEOF_VOID_P - 1);
}

#ifndef PON_HAVE_SSIZESSIZE_SLOT_TYPEDEFS
#define PON_HAVE_SSIZESSIZE_SLOT_TYPEDEFS 1
typedef PyObject *(*ssizessizeargfunc)(PyObject *, Py_ssize_t, Py_ssize_t);
typedef int (*ssizessizeobjargproc)(PyObject *, Py_ssize_t, Py_ssize_t, PyObject *);
#endif

static inline int Py_IsInitialized(void) {
    return 1;
}

/* NumPy uses Py_GenericAlias for C-level __class_getitem__ helpers.
 * Route it through the injected typeobj family so extensions and the runtime
 * stay in sync on the alias representation. */
static inline PyObject *Py_GenericAlias(PyObject *origin, PyObject *args) {
    return PyPon_Capi()->typeobj->generic_alias(origin, args);
}


/* ---- Object/container protocol completion compile surface (NumPy 2.5) ----
 * PyCFunctionObject deliberately matches the prefix of Pon's native C-function
 * carrier: NumPy and CPython macros cast PyCFunction_Type instances and read
 * m_ml/m_self directly. Descriptor structs below remain compile-surface-only
 * mirrors for CPython source compatibility.
 */

typedef struct {
    PyObject ob_base;
    PyMethodDef *m_ml;
    PyObject *m_self;
    PyObject *m_module;
    PyObject *m_weakreflist;
    void *vectorcall;
} PyCFunctionObject;

typedef struct {
    PyCFunctionObject func;
    PyTypeObject *mm_class;
} PyCMethodObject;

typedef struct {
    PyObject ob_base;
    PyObject *im_func;
    PyObject *im_self;
} PyMethodObject;

typedef struct {
    PyObject ob_base;
    PyTypeObject *d_type;
    PyObject *d_name;
    PyObject *d_qualname;
} PyDescrObject;

typedef struct {
    PyDescrObject d_common;
    PyMemberDef *d_member;
} PyMemberDescrObject;

typedef struct {
    PyDescrObject d_common;
    PyGetSetDef *d_getset;
} PyGetSetDescrObject;

typedef struct {
    PyDescrObject d_common;
    PyMethodDef *d_method;
} PyMethodDescrObject;

static PyTypeObject _PyPon_CFunction_Type_CompileOnly;
static PyTypeObject _PyPon_MemberDescr_Type_CompileOnly;
static PyTypeObject _PyPon_Range_Type_CompileOnly;

static PyTypeObject _PyPon_GetSetDescr_Type_CompileOnly;
static PyTypeObject _PyPon_MethodDescr_Type_CompileOnly;

#define PyCFunction_Type _PyPon_CFunction_Type_CompileOnly
#define PyMemberDescr_Type _PyPon_MemberDescr_Type_CompileOnly
#define PyGetSetDescr_Type _PyPon_GetSetDescr_Type_CompileOnly
#define PyMethodDescr_Type _PyPon_MethodDescr_Type_CompileOnly
#define PyRange_Type _PyPon_Range_Type_CompileOnly


static inline int PyCFunction_Check(PyObject *object) {
    return object != NULL && Py_TYPE(object) == &PyCFunction_Type;
}

static inline int PyCFunction_CheckExact(PyObject *object) {
    return PyCFunction_Check(object);
}

#define PyCFunction_GET_FUNCTION(func) (((PyCFunctionObject *)(func))->m_ml->ml_meth)
#define PyCFunction_GET_SELF(func) (((PyCFunctionObject *)(func))->m_self)

static inline int PyMethod_Check(PyObject *object) {
    (void)object;
    return 0;
}

#define PyMethod_GET_SELF(method) (((PyMethodObject *)(method))->im_self)
#define PyMethod_GET_FUNCTION(method) (((PyMethodObject *)(method))->im_func)

static inline PyObject *_PyDict_GetItem_KnownHash(PyObject *dict, PyObject *key, Py_hash_t hash) {
    (void)hash;
    return PyDict_GetItemWithError(dict, key);
}

static inline PyObject *_PyDict_NewPresized(Py_ssize_t minused) {
    (void)minused;
    return PyDict_New();
}

static inline int PyObject_HasAttrWithError(PyObject *object, PyObject *name) {
    return PyObject_HasAttr(object, name);
}

static inline int PyArg_ValidateKeywordArguments(PyObject *kwargs) {
    return kwargs == NULL || PyDict_Check(kwargs);
}

static inline int PyGC_Disable(void) {
    return 1;
}

static inline int PyGC_Enable(void) {
    return 1;
}

#ifndef PON_HAVE_PYTRACEBACK_INLINE
#define PON_HAVE_PYTRACEBACK_INLINE 1
static inline int PyTraceBack_Check(PyObject *object) {
    return PyPon_Capi()->runtime_->traceback_check(object);
}
#endif


static inline PyObject *_PyType_Lookup(PyTypeObject *type, PyObject *name) {
    return PyObject_GetAttr((PyObject *)type, name);
}

static inline const char *PyCapsule_GetName(PyObject *capsule) {
    (void)capsule;
    return NULL;
}

static inline Py_ssize_t PyUnicode_FindChar(PyObject *str, Py_UCS4 ch, Py_ssize_t start, Py_ssize_t end, int direction) {
    Py_ssize_t size = 0;
    const char *text = PyUnicode_AsUTF8AndSize(str, &size);
    if (text == NULL || ch > 0x7f) {
        return -1;
    }
    if (start < 0) {
        start = 0;
    }
    if (end > size) {
        end = size;
    }
    if (direction >= 0) {
        for (Py_ssize_t i = start; i < end; i++) {
            if ((unsigned char)text[i] == (unsigned char)ch) {
                return i;
            }
        }
    } else {
        for (Py_ssize_t i = end; i > start; i--) {
            if ((unsigned char)text[i - 1] == (unsigned char)ch) {
                return i - 1;
            }
        }
    }
    return -1;
}

static inline PyObject *PyImport_ImportModuleLevelObject(
    PyObject *name,
    PyObject *globals,
    PyObject *locals,
    PyObject *fromlist,
    int level)
{
    (void)globals;
    (void)locals;
    (void)fromlist;
    (void)level;
    return PyImport_Import(name);
}

static inline PyObject **_PyObject_GetDictPtr(PyObject *object) {
    (void)object;
    static PyObject *dict = NULL;
    return &dict;
}

static inline PyObject *PyModule_NewObject(PyObject *name) {
    const char *text = PyUnicode_AsUTF8(name);
    if (text == NULL) {
        return NULL;
    }
    PyObject *module = PyImport_AddModule(text);
    Py_XINCREF(module);
    return module;
}

static inline PyObject *PyImport_AddModuleRef(const char *name) {
    PyObject *module = PyImport_AddModule(name);
    Py_XINCREF(module);
    return module;
}

static inline PyObject *PyImport_GetModuleDict(void) {
    return PySys_GetObject("modules");
}

static inline PyObject *PyImport_GetModule(PyObject *name) {
    return PyImport_Import(name);
}

static inline void PyUnicode_InternInPlace(PyObject **object) {
    (void)object;
}

static inline int PyRange_Check(PyObject *object) {
    (void)object;
    return 0;
}

static inline Py_UCS4 PyUnicode_MAX_CHAR_VALUE(PyObject *object) {
    (void)object;
    return 0x10ffffU;
}

static inline PyObject *PyUnicode_Join(PyObject *separator, PyObject *seq) {
    return PyObject_CallMethod(separator, "join", "O", seq);
}

static inline int PyDict_Pop(PyObject *dict, PyObject *key, PyObject **result) {
    if (result == NULL) {
        PyErr_SetString(PyExc_TypeError, "PyDict_Pop result pointer must not be NULL");
        return -1;
    }
    *result = NULL;
    int found = PyDict_GetItemRef(dict, key, result);
    if (found <= 0) {
        return found;
    }
    if (PyDict_DelItem(dict, key) < 0) {
        Py_CLEAR(*result);
        return -1;
    }
    return 1;
}

/* Same rationale as PyUnstable_Object_IsUniquelyReferenced above: Pon has no
 * per-object refcount, so this CPython optimization predicate is never true.
 */
static inline int PyUnstable_Object_IsUniqueReferencedTemporary(PyObject *op) {
    (void)op;
    return 0;
}
#ifdef __cplusplus
}
#endif

#endif /* PON_PYTHON_H */
