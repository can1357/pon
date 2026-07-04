import os
r, w = os.pipe()
os.write(w, b"hello\n")
os.close(w)
f = open(r)                      # open an integer fd as a text file object
print("read:", repr(f.readline()))
f.close()
# os.fdopen path too
r2, w2 = os.pipe()
os.write(w2, b"xy")
os.close(w2)
g = os.fdopen(r2, "rb")
print("fdopen:", repr(g.read()))
g.close()
