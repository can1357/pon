def f(a, b="B", *, c="C"):
    return f"{a}:{b}:{c}"

wrapped = staticmethod(f)
print(f("A"))
print(f("A", c="K"))
print(wrapped("S"))
print(wrapped("S", c="W"))
