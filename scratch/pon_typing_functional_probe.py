from typing import NamedTuple, TypedDict
print(NamedTuple("V", [("a", int)]))
print(TypedDict("T", {"x": int}, total=False))
