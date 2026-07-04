from collections import namedtuple
from enum import Enum
class A(namedtuple('A', 'a b c d')):
    def __new__(cls, oid):
        return super().__new__(cls, 1, 2, 3, oid)
class P(A, Enum):
    X = 'x'
print(P.X, P.X.value, P.X.oid)
