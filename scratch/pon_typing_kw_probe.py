import typing
print(type(typing._make_nmtuple).__name__)
print(hasattr(typing._make_nmtuple, "__builtins__"))
print(typing._make_nmtuple("A", [], lambda format: {}, module="m"))
