# Derived from CPython v3.14.0 Lib/test/test_float.py topics (PSF license).

values = [0.0, 1.0, -1.0, 1.25, -3.5]

for value in values:
    print(format(value, "f"))

print(format(3.14159, ".2f"))
print(format(3.14159, ".4f"))
print(format(1.0 / 8.0, ".3f"))
print(format(-0.0, ".1f"))
print(format(3.5, "08.2f"))
print(format(-3.5, "08.2f"))

print(round(1.2345, 2))
print(round(2.26, 1))
print(round(-2.26, 1))
print(round(123.456, 0))

third = 1.0 / 3.0
quarter = 1.0 / 4.0
print(f"{third:.4f}")
print(f"{quarter:.4f}")
print(f"{third:08.3f}")
print(f"{-third:08.3f}")

print(str(0.01))
print(str(100.0 / 7.0))
print(repr(-0.0))
print(repr(1e-05))
