import weakref
try:
    print(weakref.WeakSet.__doc__)
except Exception as exc:
    print(type(exc).__name__, exc)
