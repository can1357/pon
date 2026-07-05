#ifndef PON_CAPI_ERR_INLINE_H
#define PON_CAPI_ERR_INLINE_H

/* Error-family wrappers added outside the legacy core_inline.h error block.
 * These are macros so err.h can expose them before Python.h defines PyPonCapi;
 * expansion happens only after the full shim has been included.
 */

#define PyErr_NormalizeException(ptype, pvalue, ptraceback) \
    (PyPon_Capi()->err->normalize_exception((ptype), (pvalue), (ptraceback)))

#define PyErr_Print() \
    (PyPon_Capi()->err->print())

/* set_sys_last_vars is intentionally ignored by Pon's C-API shim. */
#define PyErr_PrintEx(set_sys_last_vars) \
    (PyPon_Capi()->err->print_ex((set_sys_last_vars)))

#define PyErr_SetFromErrno(exception) \
    (PyPon_Capi()->err->set_from_errno((exception)))

#define PyException_SetCause(exception, cause) \
    (PyPon_Capi()->err->exception_set_cause((exception), (cause)))

#define PyException_SetContext(exception, context) \
    (PyPon_Capi()->err->exception_set_context((exception), (context)))

#define PyException_SetTraceback(exception, traceback) \
    (PyPon_Capi()->err->exception_set_traceback((exception), (traceback)))

#endif /* PON_CAPI_ERR_INLINE_H */
