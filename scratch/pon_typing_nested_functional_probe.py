from typing import TypedDict, NamedTuple
print("before")
class Outer:
    print("in outer")
    T = TypedDict("T", {"x": int}, total=False)
    N = NamedTuple("N", [("a", int)])
print("after", Outer.T, Outer.N)
