import typing as T
S = T.NewType('S', str)
def g(a, b=S('x')): return b
print(repr(g(1)))
