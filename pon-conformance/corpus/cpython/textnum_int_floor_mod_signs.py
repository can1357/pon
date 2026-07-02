# Derived from CPython v3.14.0 Lib/test/test_long.py topics (PSF license).

def show_pair(x, y):
    q = x // y
    r = x % y
    print(x, y)
    print(q, r)
    print(q * y + r)
    print(r == 0 or (r > 0) == (y > 0))


show_pair(13, 10)
show_pair(-13, 10)
show_pair(13, -10)
show_pair(-13, -10)

show_pair(12, 4)
show_pair(-12, 4)
show_pair(12, -4)
show_pair(-12, -4)

for x in [-25, -1, 0, 1, 25]:
    show_pair(x, 7)

for x in [-25, -1, 0, 1, 25]:
    show_pair(x, -7)

print(5 // 2, 5 % 2)
print(-5 // 2, -5 % 2)
print(5 // -2, 5 % -2)
print(-5 // -2, -5 % -2)
