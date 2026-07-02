# Unbound builtin-type slot methods off the TYPE object: dict/list/str
# tp_dict method surfaces, descriptor no-binding semantics, self-type-check
# TypeErrors, and the object.__eq__/__ne__ MRO-terminus fallback
# (collections.OrderedDict `dict_setitem=dict.__setitem__` default-arg
# pattern).

# --- dict slot methods reached off the type, called with explicit self
f = dict.__setitem__
d = {}
f(d, "k", 1)
print(d)
print(dict.__getitem__(d, "k"))
print(dict.get(d, "k"), dict.get(d, "missing"), dict.get(d, "missing", "dflt"))
print(dict.__contains__(d, "k"), dict.__contains__(d, "nope"))
print(dict.__len__(d))
dict.__setitem__(d, "j", 2)
dict.__delitem__(d, "j")
print(d)

# --- identity and metadata: access off the type never binds
print(dict.__setitem__ is dict.__setitem__)
print(dict.__setitem__.__name__, list.append.__name__)

# --- bound vs unbound equivalence
d2 = {}
u = dict.update
u(d2, {"a": 1})
m = d2.update
m({"b": 2})
print(d2)
print(sorted(dict.keys(d2)), sorted(dict.values(d2)), sorted(dict.items(d2)))
print(dict.setdefault(d2, "c", 3), d2["c"])
print(dict.pop(d2, "c"), sorted(dict.__iter__(d2)))
print(dict.__eq__({"x": 1}, {"x": 1}), dict.__ne__({"x": 1}, {"y": 2}))

# --- the OrderedDict default-arg pattern
def store(mapping, key, value, dict_setitem=dict.__setitem__):
    dict_setitem(mapping, key, value)

od = {}
store(od, "p", "q")
print(od)

# --- self-type mismatch raises TypeError (slot-wrapper wording)
try:
    dict.__setitem__([], "k", 1)
except TypeError as exc:
    print("TypeError", exc)
try:
    dict.__eq__([], {})
except TypeError as exc:
    print("TypeError", exc)

# --- self-type mismatch raises TypeError (method-descriptor wording)
try:
    dict.get([], "k")
except TypeError as exc:
    print("TypeError", exc)
try:
    dict.update([], {})
except TypeError as exc:
    print("TypeError", exc)
try:
    dict.__contains__([], 1)
except TypeError as exc:
    print("TypeError", exc)
try:
    list.append({}, 1)
except TypeError as exc:
    print("TypeError", exc)

# --- arity checking for unbound calls
try:
    dict.__setitem__({}, "k")
except TypeError as exc:
    print("TypeError", exc)
try:
    dict.__setitem__()
except TypeError as exc:
    print("TypeError", exc)
try:
    dict.get()
except TypeError as exc:
    print("TypeError", exc)

# --- list slot methods off the type
lst = []
list.append(lst, 5)
list.extend(lst, [7, 6])
list.insert(lst, 1, 9)
print(lst)
print(list.pop(lst), lst, list.index(lst, 9), list.count(lst, 5))

# --- str methods off the type; maketrans is static (no receiver)
print(str.upper("abc"), str.join(",", ["a", "b"]), str.startswith("hello", "he"))
maketrans = str.maketrans
print("abcda".translate(maketrans("ad", "xy")))

# --- object.__ne__/__eq__ fallback at the MRO terminus
class WithEq:
    def __init__(self, v):
        self.v = v

    def __eq__(self, other):
        return isinstance(other, WithEq) and self.v == other.v


a1, a2, a3 = WithEq(1), WithEq(1), WithEq(2)
print(WithEq.__ne__(a1, a2), WithEq.__ne__(a1, a3))
print(WithEq.__ne__(a1, "zzz") is NotImplemented)


class Plain:
    pass


p1, p2 = Plain(), Plain()
print(Plain.__ne__(p1, p1), Plain.__ne__(p1, p2) is NotImplemented)
print(object.__eq__(p1, p1), object.__eq__(p1, p2) is NotImplemented)

# --- inherited off a subclass type: MRO walk still finds dict's surface
class D(dict):
    pass


dd = D()
dict.__setitem__(dd, "s", 4)
print(dict.__getitem__(dd, "s"), dict.__contains__(dd, "s"), D.__setitem__ is dict.__setitem__)

# --- plain instance paths unaffected
pd = {"k": "v"}
pd["k2"] = "v2"
print(pd, pd.get("k"), len(pd))
