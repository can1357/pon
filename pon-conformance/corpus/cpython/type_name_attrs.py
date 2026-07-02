# type(x) identity and __name__ across builtin and class-defined types.
print(type(3).__name__)
print(type(3) is int)
print(int.__name__)
print(type(type(3)).__name__)
print(type(3.5).__name__)
print(type("s").__name__)
print(type(True).__name__)
print(type(None).__name__)

# Literal-built sequences must report the canonical builtin type.
print(type([]).__name__)
print(type(()).__name__)
print(type([]) is list)
print(type(()) is tuple)
print(type([1, 2]).__name__)
print(type((1, 2)) is tuple)
print(type(type([])).__name__)
print(list.__name__)
print(tuple.__name__)
print(type(range(3)) is range)

class Foo:
    pass

class Bar(Foo):
    pass

print(Foo.__name__)
print(Bar.__name__)
print(type(Foo()).__name__)
print(type(Bar()).__name__)
print(type(Foo()) is Foo)
print(type(Foo) is type)

# Dict keys: tuples, including tuples of type objects, hash and compare
# structurally regardless of which site built the key tuple.
d = {"a": 1}
d[(1, 2)] = 9
print(d[(1, 2)])
d[(int, str)] = 3
print(d[(int, str)])
d[(int, str)] = 4
print(d[(int, str)])
print(len(d))
d[(1, (2, (int,)))] = 5
print(d[(1, (2, (int,)))])
