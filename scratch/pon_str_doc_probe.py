s = "x"
try:
    print(s.__doc__)
except Exception as exc:
    print(type(exc).__name__, exc)
