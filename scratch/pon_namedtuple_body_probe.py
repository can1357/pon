from typing import NamedTuple
print("before class")
class V(NamedTuple):
    print("in body")
    a: int
print("after class")
