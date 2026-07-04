import typing
base = typing.NamedTuple.__mro_entries__((typing.NamedTuple,))[0]
meta = type(base)
ns = {"__module__": "m", "__annotations__": {"a": int}}
print(meta.__new__(meta, "V", (base,), ns))
