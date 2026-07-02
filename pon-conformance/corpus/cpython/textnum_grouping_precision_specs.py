# Derived from CPython v3.14.0 Lib/test/test_format.py topics (PSF license).

texts = ["abcdef", "αβγδε", "pon"]
for text in texts:
    print(format(text, ".3s"))
    print(format(text, ">6.3s"))
    print(format(text, "*^7.3s"))

floats = [1.2349, 12.0, -12.345]
for value in floats:
    print(format(value, ".1f"))
    print(format(value, ".2f"))
    print(format(value, "08.2f"))

ints = [7, 42, -42]
for value in ints:
    print(format(value, "03d"))
    print(format(value, ">5d"))
    print(format(value, "*^6d"))

print(f"{'abcdef':.3s}")
print(f"{'abcdef':>6.3s}")
print(f"{3.14159:.3f}")
print(f"{3.14159:08.2f}")
print(f"{42:05d}")
print(f"{-42:05d}")
print(f"{'pon':*^7s}")
print(f"{'pon':>6.2s}")
print(f"{1.0 / 8.0:.3f}")
print(f"{7:03d}")
