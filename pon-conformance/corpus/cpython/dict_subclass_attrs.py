# Instance attributes, method-override dispatch, super() delegation, and
# native interop for classes deriving from dict.


# --- instance attributes + overrides + super() delegation
class Recorder(dict):
    def __init__(self):
        super().__init__()
        self.log = []

    def __setitem__(self, key, value):
        self.log.append(key)
        super().__setitem__(key, value.upper() if isinstance(value, str) else value)

    def __getitem__(self, key):
        self.log.append("get:" + str(key))
        return super().__getitem__(key)


r = Recorder()
r["a"] = "hi"
r["b"] = 3
print(r["a"], r["b"])
print(r.log)
print(len(r), bool(r))
print(sorted(r.keys()))

# --- plain instance attrs, super().__init__(mapping), dict(subclass) interop
class Bag(dict):
    def __init__(self, seed):
        super().__init__(seed)
        self.tag = "bag"


b = Bag({"x": 1})
b.extra = 41
b.extra += 1
print(b.tag, b.extra, b["x"], dict(b))

# --- isinstance / issubclass
print(isinstance(b, dict), isinstance(b, Bag), isinstance({}, Bag))
print(issubclass(Bag, dict), issubclass(dict, Bag))

# --- native dict methods through the subclass
b["y"] = 2
print(b.get("x"), b.get("missing", "dflt"))
print(b.setdefault("z", 3), sorted(b.values()))
print(b.pop("z"), len(b))
b.update({"w": 9})
print(sorted(b.keys()))
print("x" in b, "nope" in b)

# --- equality both directions against plain dicts
print(b == {"x": 1, "y": 2, "w": 9}, b != {"x": 1}, {"x": 1, "y": 2, "w": 9} == b)

# --- truthiness of an empty subclass
empty = Bag({})
print(bool(empty), len(empty))

# --- iteration order, del, repr interop
d2 = Bag({})
d2["one"] = 1
d2["two"] = 2
print([k for k in d2])
del d2["one"]
print(list(d2), d2)

# --- KeyError propagation through subclass access
try:
    _ = d2["one"]
except KeyError as exc:
    print("KeyError", exc)

# --- grandchild: override chain through two levels of super()
class Shouty(Recorder):
    def __getitem__(self, key):
        return super().__getitem__(key)


s = Shouty()
s["q"] = "soft"
print(s["q"], s.log)
print(isinstance(s, Recorder), isinstance(s, dict))

# --- plain dict behavior unaffected
p = {"k": "v"}
p["k2"] = "v2"
print(p, len(p), isinstance(p, dict))
