b = "abcd".encode()
mv = memoryview(b)
print(len(mv), mv[1], mv[1:3].tobytes().decode(), mv.readonly)
ba = bytearray("wxyz", "ascii")
mw = memoryview(ba)
print(len(mw), mw[2], mw[1:3].tobytes().decode(), mw.readonly)
mw[1] = 65
print(ba.decode(), mw.tobytes().decode())
part = mw[1:3]
part[0] = 66
print(ba.decode(), part.tobytes().decode())
try:
    mv[0] = 65
    print("writable")
except TypeError:
    print("readonly")
