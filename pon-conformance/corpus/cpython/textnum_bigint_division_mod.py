# Derived from CPython v3.14.0 Lib/test/test_long.py topics (PSF license).

def show_identity(x, y):
    q = x // y
    r = x % y
    print(y, r)
    print(x - (q * y + r))
    print(r == 0 or (r > 0) == (y > 0))


big = (1 << 80) + 13
near = (1 << 72) - 5
wide = (10 ** 35) + 98765

show_identity(big, 97)
show_identity(big, -97)
show_identity(-big, 97)
show_identity(-big, -97)

show_identity(near, 101)
show_identity(near, -101)
show_identity(-near, 101)
show_identity(-near, -101)

show_identity(wide, 12345)
show_identity(wide, -12345)
show_identity(-wide, 12345)
show_identity(-wide, -12345)

print(((1 << 100) - 1) % 31)
print((-(1 << 100) + 1) % 31)
print(((10 ** 30) + 1) % 9)
print((-(10 ** 30) - 1) % 9)
