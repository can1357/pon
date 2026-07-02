# Derived from CPython v3.14.0 Lib/test/test_slice.py topics (PSF license).

items = [0, 1, 2, 3, 4, 5, 6, 7]
text = "abcdefgh"
unicode = "αβγδεζηθ"

print(items[1:6:2])
print(items[::3])
print(items[::-1])
print(items[6:1:-2])
print(items[-20:20:3])
print(items[3:3])
print(items[:])

print(text[2:])
print(text[:3])
print(text[::2])
print(text[::-1])
print(text[6:1:-2])
print(text[-20:20:3])

print(unicode[1:6:2])
print(unicode[::-1])
print(unicode[:4])
print(unicode[4:])

start = 1
stop = 7
step = 3
print(items[start:stop:step])
print(text[start:stop:step])
print(unicode[start:stop:step])

print(items[True:6:2])
print(text[False:True + 4])
