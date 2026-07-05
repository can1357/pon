import zlib
data = b'hello world ' * 50
c = zlib.compress(data, level=9)
print(len(c) < len(data), zlib.decompress(c) == data)
c2 = zlib.compress(data)
print(zlib.decompress(c2) == data)
c3 = zlib.compress(data, 1)
print(zlib.decompress(c3) == data)
print(zlib.compress.__module__ if hasattr(zlib.compress, '__module__') else 'n/a')
try:
    zlib.compress(data, level=99)
except Exception as e:
    print(type(e).__name__)
import bz2
print(len(bz2.compress(data, compresslevel=9)) < len(data))
