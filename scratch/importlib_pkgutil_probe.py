import importlib
print('before', 'util' in dir(importlib), hasattr(importlib,'util'), '_abc' in dir(importlib), hasattr(importlib,'_abc'))
try:
    import importlib.util
    print('util import ok', 'util' in dir(importlib), hasattr(importlib,'util'))
except Exception as exc:
    print('util import err', type(exc).__name__, exc)
try:
    import importlib._abc
    print('_abc import ok', '_abc' in dir(importlib), hasattr(importlib,'_abc'))
except Exception as exc:
    print('_abc import err', type(exc).__name__, exc)
try:
    import pkgutil
    print('pkgutil ok', pkgutil.ModuleInfo('x','y',False).name)
except Exception as exc:
    print('pkgutil err', type(exc).__name__, exc)
