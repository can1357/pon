from typing import NamedTuple, TypedDict

class V(NamedTuple):
    a: int
    b: int

print(V(1, 2))
print(V.__mro__)

class T(TypedDict, total=False):
    x: int

print(T)
