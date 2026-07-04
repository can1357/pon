from Cython.Compiler import Errors


def traced_report_error(err, use_stack=True):
    print("TRACE_REPORT", type(err).__name__, getattr(err, "message", str(err)))
    raise err


def traced_error(position, message):
    print("TRACE_ERROR", position, message)
    raise Errors.CompileError(position, message)

Errors.report_error = traced_report_error
Errors.error = traced_error

import Cython.Compiler.ParseTreeTransforms as ParseTreeTransforms
print("parse_tree_module", ParseTreeTransforms.__name__)
