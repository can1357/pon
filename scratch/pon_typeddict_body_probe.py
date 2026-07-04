from typing import TypedDict
print("before class")
class T(TypedDict, total=False):
    print("in body")
    x: int
print("after class")
