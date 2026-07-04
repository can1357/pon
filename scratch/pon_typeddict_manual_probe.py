import typing
base = typing.TypedDict.__mro_entries__((typing.TypedDict,))[0]
meta = type(base)
ns = {"__module__": "m", "__annotations__": {"x": int}}
print(meta.__new__(meta, "T", (base,), ns, total=False))
