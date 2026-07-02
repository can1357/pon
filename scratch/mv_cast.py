b = bytearray(256)
for i in range(256): b[i] = i % 256
a = memoryview(bytes(b)).cast("I")
print(len(a), a.itemsize, a.readonly)
print(a.tolist()[:4])
print(a[0], a[63])
w = memoryview(b)
print(len(w), w.itemsize, w.readonly, w[255])
