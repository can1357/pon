import weakref
x = weakref.WeakSet()
try:
    print(x.__doc__)
except Exception as exc:
    print(type(exc).__name__, exc)
