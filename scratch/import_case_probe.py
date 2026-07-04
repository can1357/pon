import Cython
import cython
print(Cython.__name__, hasattr(Cython, 'declare'))
print(cython.__name__, hasattr(cython, 'declare'))
print(Cython is cython)
