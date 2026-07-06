from enum import Enum
from collections import namedtuple

class ASN(namedtuple("ASN", "nid shortname longname oid")):
    __slots__ = ()

    def __new__(cls, oid):
        print('ASN new', cls, oid)
        return super().__new__(cls, 1, 'short', 'long', oid)

class Purpose(ASN, Enum):
    SERVER_AUTH = '1.2.3'

print(Purpose.SERVER_AUTH)
