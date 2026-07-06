from enum import Enum
from collections import namedtuple

class ASN(namedtuple("ASN", "nid shortname longname oid")):
    __slots__ = ()

    def __new__(cls, oid):
        print('function qualname', ASN.__new__.__qualname__)
        print('cls.__new__', cls.__new__, getattr(cls.__new__, '__qualname__', None))
        snew = super().__new__
        print('super new', snew, getattr(snew, '__qualname__', None))
        return snew(cls, 1, 'short', 'long', oid)

class Purpose(ASN, Enum):
    SERVER_AUTH = '1.2.3'
