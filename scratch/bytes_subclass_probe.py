import re

class B(bytes):
    def __new__(cls, value=b"", encoding="latin-1"):
        self = bytes.__new__(cls, value)
        self.encoding = encoding
        return self

b = B(b"abc", "custom")
empty = B()
print("class", type(b).__name__, isinstance(b, bytes), b.encoding)
print("len-bool", len(b), bool(b), bool(empty))
print("contains", 97 in b, b"b" in b, b"z" in b)
print("iteration", list(b))
print("indexing", b[0], b[-1], b[1:])
print("bytes-roundtrip", bytes(B(b"x")), type(bytes(B(b"x"))).__name__)
print("equality", b"abc" == b, b == b"abc")
print("re-sub", re.sub(b"a", b"X", B(b"aba")))
print("re-match", re.match(b"a.", B(b"abc")).group(0))
print("re-findall", re.findall(b"a.", B(b"abc aba")))

class I(int):
    pass

class S(str):
    pass

print("sanity", int(I(7)) + 1, len(S("xy")), S("xy") == "xy")
