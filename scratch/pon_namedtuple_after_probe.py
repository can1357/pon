from typing import NamedTuple
print("before")
class V(NamedTuple):
    a: int
print("after class")
print(V)
print(V(1))
