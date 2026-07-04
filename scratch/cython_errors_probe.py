import Cython.Compiler.Errors as E
E.init_thread()
print('stack type:', type(E.threadlocal.cython_errors_stack).__name__)
held = E.hold_errors()
print('held type:', type(held).__name__)
E.release_errors(ignore=True)
print('released ok')
E.reset()
print('reset ok')
import Cython.Compiler.ParseTreeTransforms as P
print('PTT ok')
