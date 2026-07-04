import typing
print(type(typing.NamedTuple).__name__)
print(hasattr(typing.NamedTuple, "__mro_entries__"))
print(type(typing.TypedDict).__name__)
print(hasattr(typing.TypedDict, "__mro_entries__"))
