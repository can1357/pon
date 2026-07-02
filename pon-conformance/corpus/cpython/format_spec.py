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
print("{x} {y}".format(x=1, y="kw"))
print("{0}+{n}".format("pos", n=2))
print("{a}{b}{a}".format(a="-", b="mid"))
print("{} {} {x}".format(1, 2, x=3))
print("{x:>{w}}".format(x="v", w=5))
print("{n[1]}.{o.imag}".format(n=[7, 8], o=1 + 2j))
kw = {"k": "starstar", "n": 9}
print("{k}/{n}".format(**kw))
print("{0} {x} {y}".format("p", x=1, **{"y": 2}))
print("{v!r:^9}|".format(v="q"))
try:
    "{missing}".format(x=1)
except KeyError as exc:
    print("KeyError:", exc)
try:
    "{m[zz]}".format(m={"a": 1})
except KeyError as exc:
    print("KeyError:", exc)
try:
    "{missing}".format_map({"a": 1})
except KeyError as exc:
    print("KeyError:", exc)
print(format(CustomFormat(), "token"))
