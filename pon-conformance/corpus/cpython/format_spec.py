class Box:
    def __init__(self, x):
        self.x = x

class CustomFormat:
    def __format__(self, spec):
        return "custom[" + spec + "]"

box = Box("attr")
items = ["zero", "one", "two"]

print(f"{'abc':*>6s}|{'abcdef':.3s}|{'x':05}")
print(f"{255:#06x}|{255:#_b}|{65:c}|{1234567:,d}|{1234567:_d}")
print(f"{-0.0:z.1f}|{0.125:.1%}|{12345.678:,.2f}|{12.5:010.2f}")
print(f"{3.14159:{8}.{2}f}|{42:+05d}|{42: d}")
print("{0.x}:{1[1]}:{2!r:^8}:{3:{4}.{5}f}".format(box, items, "xy", 3.14159, 8, 2))
print(format(CustomFormat(), "token"))
