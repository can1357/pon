from enum import Enum
from collections import namedtuple

Base = namedtuple("ASN", "nid shortname longname oid")
print('Base', Base, Base.__new__)

class ASN(Base):
    __slots__ = ()

    def __new__(cls, oid):
        print('ASN mro', ASN.__mro__)
        print('cls mro', cls.__mro__)
        print('super new', super().__new__)
        return super().__new__(cls, 1, 'short', 'long', oid)

print('ASN new func', ASN.__new__)
class Purpose(ASN, Enum):
    SERVER_AUTH = '1.2.3'

print(Purpose.SERVER_AUTH)
