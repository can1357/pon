from Cython.Compiler import Errors
Errors.init_thread()
print("stack_before", hasattr(Errors.threadlocal, "cython_errors_stack"), len(Errors.threadlocal.cython_errors_stack))
import Cython.Compiler.ParseTreeTransforms as ParseTreeTransforms
print("parse_tree_module", ParseTreeTransforms.__name__)
print("stack_after", hasattr(Errors.threadlocal, "cython_errors_stack"), len(Errors.threadlocal.cython_errors_stack))
