# object.__ne__ fallback semantics
class WithEq:
    def __init__(self, v):
        self.v = v
    def __eq__(self, other):
        return isinstance(other, WithEq) and self.v == other.v

a1, a2, a3 = WithEq(1), WithEq(1), WithEq(2)
print(WithEq.__ne__(a1, a2), WithEq.__ne__(a1, a3))
print(WithEq.__ne__(a1, "zzz"))

class Plain:
    pass

p1, p2 = Plain(), Plain()
print(Plain.__ne__(p1, p1), Plain.__ne__(p1, p2))
print(object.__eq__(p1, p1), object.__eq__(p1, p2))

# str unbound methods + maketrans roundtrip
print(str.upper("abc"), str.join(",", ["a", "b"]), str.startswith("hello", "he"))
mt = str.maketrans
print("abcda".translate(mt("ad", "xy")))
print(str.maketrans("ab", "cd") is not None)

# dict views via unbound access
d = {"b": 2, "a": 1}
print(sorted(dict.keys(d)), sorted(dict.values(d)), sorted(dict.items(d)))
print(dict.setdefault(d, "c", 3), d["c"])
it = dict.__iter__(d)
print(sorted(it))
print(dict.__eq__({"x": 1}, {"x": 1}), dict.__ne__({"x": 1}, {"y": 2}))
print(dict.__eq__({}, 3))
