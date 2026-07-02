# Derived from CPython v3.14.0 Lib/test/test_fstring.py topics (PSF license).

name = "pon"
value = 42
width = 6
precision = 3
ratio = 1.0 / 3.0

print(f"{name}:{value:04d}:{value * 2}")
print(f"left={name:<6s}!")
print(f"right={name:>6s}!")
print(f"center={name:*^7s}!")
print(f"float={ratio:.4f}")
print(f"wide={value:{width}d}!")
print(f"text={name!r:8s}!")
print(f"ascii={'é'!a}")

for item in [0, 7, 42]:
    print(f"item={item:03d}")

for word in ["alpha", "βeta", "gamma"]:
    print(f"{word:.3s}:{word:>6s}")

nested_width = 5
print(f"nested={7:{nested_width}d}")
print(f"neg={-7:04d}")
print(f"plain braces {{value}} -> {value}")
print(f"sum={value + 8:04d}")
print(f"repr={name!r}")
print(f"str={name!s}")
