# Derived from CPython v3.14.0 Lib/test/test_long.py topics (PSF license).

base = 1 << 80
bigger = 1 << 120
mask40 = (1 << 40) - 1
mask60 = (1 << 60) - 1

print((base + 12345) - base)
print((base - 12345) - base)
print((base + 12345) % 100000)
print((base - 12345) % 100000)

product = ((1 << 40) - 1) * ((1 << 40) + 1)
expected = (1 << 80) - 1
print(product - expected)
print(product % 97)

square = (1 << 70) * (1 << 70)
print((square >> 135) - 32)
print(square % 31)

mixed = (mask40 * mask60) + base
check = (mask40 * mask60) - ((1 << 100) - (1 << 60) - (1 << 40) + 1)
print(check)
print((mixed - base) % 997)

power = 10 ** 40
print((power + 77) - power)
print((power - 77) % 1000)
print((2 ** 100) >> 96)
print(((2 ** 100) + (2 ** 50)) % 1009)
print(((bigger >> 100) - (1 << 20)))
