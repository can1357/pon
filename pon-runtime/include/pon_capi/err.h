#ifndef PON_CAPI_ERR_H
#define PON_CAPI_ERR_H

/* Error family: exception raising, inspection, and exception-type
 * singletons. Singleton fields hold REAL runtime exception type objects;
 * they are valid arguments to any API taking a type or class.
 */


/* Pon's boxed BaseException payload. This is a source-compatibility mirror of
 * the runtime layout for C code that inspects exception chaining fields; it is
 * not CPython's binary ABI.
 */
typedef struct PyBaseExceptionObject {
    PyObject ob_base;
    PyObject *message;
    PyObject *cause;
    PyObject *context;
    PyObject *traceback;
    PyObject *args;
    void *dict;
    unsigned char suppress_context;
} PyBaseExceptionObject;

typedef struct PyPonCapiErr {
    void (*set_string)(PyObject *, const char *);
    void (*set_object)(PyObject *, PyObject *);
    void (*set_none)(PyObject *);
    PyObject *(*occurred)(void);
    void (*clear)(void);

    PyObject *exc_base_exception;
    PyObject *exc_exception;
    PyObject *exc_runtime_error;
    PyObject *exc_type_error;
    PyObject *exc_value_error;
    PyObject *exc_import_error;
    PyObject *exc_overflow_error;
    PyObject *exc_index_error;
    PyObject *exc_key_error;
    PyObject *exc_attribute_error;
    PyObject *exc_not_implemented_error;
    PyObject *exc_stop_iteration;
    PyObject *exc_memory_error;
    PyObject *exc_os_error;
    PyObject *exc_system_error;
    PyObject *exc_buffer_error;
    PyObject *exc_zero_division_error;
    PyObject *exc_arithmetic_error;
    PyObject *exc_floating_point_error;
    PyObject *exc_deprecation_warning;
    PyObject *exc_runtime_warning;
    PyObject *exc_user_warning;
    PyObject *exc_lookup_error;
    int (*exception_matches)(PyObject *);
    int (*given_exception_matches)(PyObject *, PyObject *);
    void (*fetch)(PyObject **, PyObject **, PyObject **);
    void (*restore)(PyObject *, PyObject *, PyObject *);
    int (*warn_ex)(PyObject *, const char *, Py_ssize_t);
    void (*write_unraisable)(PyObject *);
    void (*normalize_exception)(PyObject **, PyObject **, PyObject **);
    void (*print)(void);
    void (*print_ex)(int);
    PyObject *(*set_from_errno)(PyObject *);
    void (*exception_set_cause)(PyObject *, PyObject *);
    void (*exception_set_context)(PyObject *, PyObject *);
    int (*exception_set_traceback)(PyObject *, PyObject *);
    PyObject *exc_warning;
    PyObject *exc_future_warning;
    PyObject *exc_import_warning;
    PyObject *exc_module_not_found_error;
    PyObject *exc_assertion_error;
    PyObject *exc_name_error;
    PyObject *exc_unicode_error;
    PyObject *exc_unicode_encode_error;
    PyObject *exc_unicode_decode_error;
    PyObject *exc_recursion_error;
    PyObject *(*new_exception)(const char *, PyObject *, PyObject *);
    int (*check_signals)(void);
    /* Family expansion point: append fields only; never reorder. */
} PyPonCapiErr;


#include "err_inline.h"
#endif /* PON_CAPI_ERR_H */
