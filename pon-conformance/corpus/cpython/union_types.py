u = int | str
v = str | int
w = int | str | int
x = int | (str | None)
y = (int | str) | (str | float)

print(repr(u))
print(repr(v))
print(repr(w))
print(repr(x))
print(repr(y))
print(u == v, w == v, hash(u) == hash(v))
print(isinstance(1, u), isinstance('x', u), isinstance(1.0, u))
print(tuple(arg.__name__ for arg in x.__args__))
print(repr(int | int))
