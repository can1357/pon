charmap = bytearray(256)
charmap[97] = 1
BITS = b'0' + b'1' * 255
t = charmap.translate(BITS)
print("translate ok", len(t))
s = t[::-1]
print("revslice ok")
v = int(s[0:16], 2)
print("int ok", v)
mapping = bytearray(4)
m = memoryview(bytes(mapping)).cast('I')
print("cast ok", m.tolist())
