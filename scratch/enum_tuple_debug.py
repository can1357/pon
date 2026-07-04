from collections import namedtuple
from enum import Enum
Base = namedtuple('A', 'a b c d')
class A(Base):
    def __new__(cls, oid):
        print('A.__new__ cls', cls, 'mro', cls.__mro__)
        target = super().__new__
        print('target', target)
        return target(cls, 1, 2, 3, oid)
print('A mro', A.__mro__)
print('direct', A('x'))
class P(A, Enum):
    X = 'x'
print(P.X)
