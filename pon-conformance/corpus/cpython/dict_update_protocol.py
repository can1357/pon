# dict.update protocol shapes and error messages.


def show(label, value):
    print(label, value)


d = {"base": 0}
show("exact before", sorted(d.items()))
show("exact return", d.update({"a": 1, "b": 2}))
show("exact after", sorted(d.items()))

pairs = {}
show("pairs return", pairs.update([("x", 3), ["y", 4]]))
show("pairs after", sorted(pairs.items()))


def gen_pairs():
    yield "g1", 5
    yield "g2", 6


generated = {}
show("generator return", generated.update(gen_pairs()))
show("generator after", sorted(generated.items()))


class MappingLike:
    def keys(self):
        return ["m1", "m2"]

    def __getitem__(self, key):
        return "value-" + key


mapped = {}
show("mapping return", mapped.update(MappingLike()))
show("mapping after", sorted(mapped.items()))

zero = {"z": 9}
show("zero return", zero.update())
show("zero after", sorted(zero.items()))

for label, operation in (
    ("short element", lambda: {}.update([1, 2])),
    ("long element", lambda: {}.update([(1, 2, 3)])),
):
    try:
        operation()
    except Exception as exc:
        print(label, type(exc).__name__, str(exc))

globals().update((("G", 1),))
print("global", G)
