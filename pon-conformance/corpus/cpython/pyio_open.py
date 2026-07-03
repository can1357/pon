# Direct _pyio.open coverage: in 3.14 the module-level carrier is a callable
# staticmethod object, and binary reads flow through FileIO.readall/readinto.

import _pyio

b = bytearray(b"abc")
b.resize(5)
print(len(b), b)
b.resize(2)
print(len(b), b)


path = "target/pon_pyio_open_corpus.bin"
with open(path, "wb") as f:
    print(f.write(b"alpha\nbeta"))

print(type(_pyio.open).__name__)
print(callable(_pyio.open))

with _pyio.open(path, "rb") as f:
    print(type(f).__name__)
    print(f.read())

with _pyio.open(path, "rb") as f:
    buf = bytearray(5)
    print(f.readinto(buf), bytes(buf))
