# int() parsing canaries: prefixes, bases, signs, underscores, whitespace, bool base, float truncation.

def show(text, base):
    try:
        print("parse", repr(text), repr(base), int(text, base))
    except ValueError as exc:
        print("parse-error", repr(text), repr(base), type(exc).__name__, str(exc))


show("0b1010", 0)
show("0o755", 0)
show("0xFf", 0)
show("123", 0)
show("   +42  ", 0)
show("  -0b1_010  ", 0)
show("1_234_567", 10)
show("101010", 2)
show("755", 8)
show("ff", 16)
show("z", 36)
show("10", 35)
for base in range(2, 37):
    show("10", base)
show("10", False)
show("10", True)
show("2", 2)
show("1__2", 10)

print("trunc", int(3.9), int(-3.9), int(0.0), int(-0.0))
