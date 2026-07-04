import cython
x = cython.declare(frozenset, frozenset(('__cinit__', '__dealloc__')))
y = cython.declare(object, {'a': None})
print(type(x).__name__, y)
