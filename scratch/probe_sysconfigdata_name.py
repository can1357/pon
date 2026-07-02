import sys
print(repr(sys.abiflags))
print(repr(sys.platform))
print(repr(getattr(sys.implementation, '_multiarch', '')))
name = f'_sysconfigdata_{sys.abiflags}_{sys.platform}_{getattr(sys.implementation, "_multiarch", "")}'
print(name)
try:
    import importlib
    importlib.import_module(name)
    print('IMPORT OK')
except ImportError as exc:
    print('IMPORT FAIL:', type(exc).__name__, exc)
