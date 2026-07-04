from Cython.Compiler import Errors
Errors.init_thread()
print("before_main", hasattr(Errors.threadlocal, "cython_errors_stack"), len(Errors.threadlocal.cython_errors_stack))
from Cython.Compiler.Main import setuptools_main
print("main_imported", setuptools_main.__name__)
print("after_main", hasattr(Errors.threadlocal, "cython_errors_stack"), len(Errors.threadlocal.cython_errors_stack))
