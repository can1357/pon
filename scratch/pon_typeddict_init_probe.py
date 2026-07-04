import typing
base = typing.TypedDict.__mro_entries__((typing.TypedDict,))[0]
meta = type(base)
ns = {"__module__": "m", "__annotations__": {"x": int}}
T = meta.__new__(meta, "T", (base,), ns, total=False)
print(T)
try:
    T.__init_subclass__(total=False)
except Exception as exc:
    print(type(exc).__name__, exc)
