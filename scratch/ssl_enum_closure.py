from collections import namedtuple

class ASN(namedtuple("ASN", "nid shortname longname oid")):
    __slots__ = ()

    def __new__(cls, oid):
        print('__closure__', ASN.__new__.__closure__)
        if ASN.__new__.__closure__:
            print('cells', [c.cell_contents for c in ASN.__new__.__closure__])
        return super().__new__(cls, 1, 'short', 'long', oid)

print(ASN('x'))
