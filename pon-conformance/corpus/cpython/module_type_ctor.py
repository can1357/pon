# types.ModuleType construction: the module type's tp_new must build a real,
# fully-initialized module object (live attrs map, CPython __init__ namespace
# seed) instead of falling through the generic heap-instance allocator, and a
# synthetic module named like a REAL module must never alias the real
# module's namespace (registry identity is per-instance, not per-name).
import gc
import types

# Construction + __name__/__doc__ binding (positional and keyword doc).
m = types.ModuleType('x')
print(type(m).__name__, m.__name__, m.__doc__)
d = types.ModuleType('y', 'docstring')
print(d.__name__, d.__doc__)
k = types.ModuleType('z', doc='kwdoc')
print(k.__name__, k.__doc__)

# CPython module.__init__ seeds exactly these names.
print(sorted(dir(m)))
print(m.__package__, m.__loader__, m.__spec__)

# setattr/getattr/dir round-trip through both spellings, plus delete.
m.a = 1
setattr(m, 'b', [2, 3])
print(m.a, getattr(m, 'b'), hasattr(m, 'a'))
print('a' in dir(m), 'b' in dir(m))
del m.a
delattr(m, 'b')
print('a' in dir(m), 'b' in dir(m), hasattr(m, 'a'))
try:
    m.missing
except AttributeError as e:
    print('AttributeError', "'missing'" in str(e))

# The namespace view is live and writes through it resolve as attributes.
m.c = 'attr'
ns = vars(m)
print(ns is m.__dict__, ns['__name__'], ns['c'])
ns['via_dict'] = 41
print(m.via_dict, 'via_dict' in dir(m))

# Same-name-as-real-module isolation: a synthetic 'os' neither reads the
# real module's namespace nor pollutes it.
import os

fake = types.ModuleType('os')
try:
    fake.getcwd
    print('BAD: synthetic os sees real getcwd')
except AttributeError as e:
    print('AttributeError', "'getcwd'" in str(e))
fake.sep = 'FAKE'
print(os.sep == 'FAKE', fake.sep, os.path.join('a', 'b'))
print('getcwd' in dir(fake), 'sep' in dir(fake), 'sep' in dir(os))

# Two same-named synthetic modules stay distinct objects with distinct
# namespaces (per-identity, not per-name), and attr values survive a
# collection (synthetic module attrs are GC roots).
a = types.ModuleType('twin')
b = types.ModuleType('twin')
a.v = [1]
b.v = [2]
gc.collect()
print(a is b, a.v, b.v)

# Synthetic construction never registers the module for import machinery.
import sys

print('never_imported_zz' in sys.modules)
types.ModuleType('never_imported_zz')
print('never_imported_zz' in sys.modules)

# Constructor error surface.
for bad in (lambda: types.ModuleType(),
            lambda: types.ModuleType(1),
            lambda: types.ModuleType('a', 'b', 'c'),
            lambda: types.ModuleType('a', bogus=1)):
    try:
        bad()
    except TypeError as e:
        print('TypeError:', e)
