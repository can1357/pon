from collections import namedtuple
class A(namedtuple('A', 'x y')):
    def __new__(cls, z):
        return super().__new__(cls, 1, 2)
print(A(0))
