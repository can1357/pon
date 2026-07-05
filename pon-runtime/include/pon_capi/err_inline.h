#ifndef PON_CAPI_ERR_INLINE_H
#define PON_CAPI_ERR_INLINE_H

/* Error-family wrappers added outside the legacy core_inline.h error block.
 * These are macros so err.h can expose them before Python.h defines PyPonCapi;
 * expansion happens only after the full shim has been included.
 */

#include <stdlib.h>

static inline PyObject *PyErr_NoMemory(void);
static inline int PyErr_WarnEx(PyObject *category, const char *message, Py_ssize_t stack_level);
static inline Py_ssize_t _PyPon_FormatUnicodeInto(char *out, Py_ssize_t capacity, const char *format, va_list vargs);

#define PyExc_Warning (PyPon_Capi()->err->exc_warning)
#define PyExc_FutureWarning (PyPon_Capi()->err->exc_future_warning)
#define PyExc_ImportWarning (PyPon_Capi()->err->exc_import_warning)
#define PyExc_ModuleNotFoundError (PyPon_Capi()->err->exc_module_not_found_error)
#define PyExc_AssertionError (PyPon_Capi()->err->exc_assertion_error)
#define PyExc_NameError (PyPon_Capi()->err->exc_name_error)
#define PyExc_UnicodeError (PyPon_Capi()->err->exc_unicode_error)
#define PyExc_UnicodeEncodeError (PyPon_Capi()->err->exc_unicode_encode_error)
#define PyExc_UnicodeDecodeError (PyPon_Capi()->err->exc_unicode_decode_error)
#define PyExc_RecursionError (PyPon_Capi()->err->exc_recursion_error)
#define PyExc_GeneratorExit (PyPon_Capi()->err->exc_generator_exit)
#define PyExc_StopAsyncIteration (PyPon_Capi()->err->exc_stop_async_iteration)
#define PyExc_UnboundLocalError (PyPon_Capi()->err->exc_unbound_local_error)


#define PyErr_NewException(name, base, dict) \
    (PyPon_Capi()->err->new_exception((name), (base), (dict)))

#define PyErr_CheckSignals() \
    (PyPon_Capi()->err->check_signals())

static inline int PyErr_WarnFormat(PyObject *category, Py_ssize_t stack_level, const char *format, ...) {
    const char *safe_format = format == NULL ? "" : format;
    va_list measure_args;
    va_start(measure_args, format);
    Py_ssize_t needed = _PyPon_FormatUnicodeInto(NULL, 0, safe_format, measure_args);
    va_end(measure_args);
    if (needed < 0) {
        return -1;
    }
    char stack[512];
    char *buffer = stack;
    if (needed >= (Py_ssize_t)sizeof(stack)) {
        buffer = (char *)malloc((size_t)needed + 1);
        if (buffer == NULL) {
            PyErr_NoMemory();
            return -1;
        }
    }
    va_list render_args;
    va_start(render_args, format);
    Py_ssize_t written = _PyPon_FormatUnicodeInto(buffer, needed + 1, safe_format, render_args);
    va_end(render_args);
    if (written < 0) {
        if (buffer != stack) {
            free(buffer);
        }
        return -1;
    }
    int result = PyErr_WarnEx(category, buffer, stack_level);
    if (buffer != stack) {
        free(buffer);
    }
    return result;
}

#define PyErr_NormalizeException(ptype, pvalue, ptraceback) \
    (PyPon_Capi()->err->normalize_exception((ptype), (pvalue), (ptraceback)))

#define PyErr_Print() \
    (PyPon_Capi()->err->print())

/* set_sys_last_vars is intentionally ignored by Pon's C-API shim. */
#define PyErr_PrintEx(set_sys_last_vars) \
    (PyPon_Capi()->err->print_ex((set_sys_last_vars)))

#define PyErr_SetFromErrno(exception) \
    (PyPon_Capi()->err->set_from_errno((exception)))

#define PyErr_SetRaisedException(exception) \
    (PyPon_Capi()->err->set_raised_exception((exception)))

#define PyException_SetCause(exception, cause) \
    (PyPon_Capi()->err->exception_set_cause((exception), (cause)))

#define PyException_SetContext(exception, context) \
    (PyPon_Capi()->err->exception_set_context((exception), (context)))

#define PyException_SetTraceback(exception, traceback) \
    (PyPon_Capi()->err->exception_set_traceback((exception), (traceback)))

#endif /* PON_CAPI_ERR_INLINE_H */
