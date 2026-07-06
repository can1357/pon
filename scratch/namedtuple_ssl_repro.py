from collections import namedtuple

class A(namedtuple("A", "a b c d")):
    __slots__ = ()

    def __new__(cls, x):
        return super().__new__(cls, 1, 2, 3, 4)

print(A("x"))
