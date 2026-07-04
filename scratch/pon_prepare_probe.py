import typing
for base in [typing.NamedTuple, typing.TypedDict]:
    repl = base.__mro_entries__((base,))[0]
    meta = type(repl)
    print(base.__name__, meta.__name__, hasattr(meta, "__prepare__"), hasattr(meta.__dict__, "get"))
    d = meta.__dict__
    print("prepare in dict", "__prepare__" in d if hasattr(d, "__contains__") else None)
