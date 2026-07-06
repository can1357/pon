from enum import Enum
from collections import namedtuple

Base = namedtuple("ASN", "nid shortname longname oid")
class ASN(Base):
    __slots__ = ()
    def __new__(cls, oid):
        print('sub dict new', ASN.__dict__['__new__'])
        print('base dict new', Base.__dict__['__new__'])
        print('super new', super().__new__)
        return super().__new__(cls, 1, 'short', 'long', oid)

class Purpose(ASN, Enum):
    SERVER_AUTH = '1.2.3'
