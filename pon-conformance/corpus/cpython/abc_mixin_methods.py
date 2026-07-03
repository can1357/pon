import _collections_abc


class MapStub(_collections_abc.MutableMapping):
    """MutableMapping with only the 5 abstract stubs: every other method is
    a Python-defined mixin inherited through the ABC MRO."""

    def __init__(self):
        self._d = {}

    def __getitem__(self, key):
        return self._d[key]

    def __setitem__(self, key, value):
        self._d[key] = value

    def __delitem__(self, key):
        del self._d[key]

    def __iter__(self):
        return iter(self._d)

    def __len__(self):
        return len(self._d)


m = MapStub()
print("map mro", [c.__name__ for c in type(m).__mro__])
m["a"] = 1
m.update({"b": 2}, c=3)
m.update([("d", 4)])
print("map update", sorted(m.items()))
print("map setdefault", m.setdefault("e", 5), m.setdefault("a", 99))
print("map pop", m.pop("b"), m.pop("zz", "dflt"))
print("map popitem in", m.popitem()[0] in "acde")
print("map keys", sorted(m.keys()))
print("map values", sorted(m.values()))
print("map contains", "a" in m, "zz" in m)
print("map get", m.get("a"), m.get("zz"), m.get("zz", "gd"))
print("map eq", m == dict(m), m != dict(m))
m.clear()
print("map clear", len(m), list(m.items()))


class SeqStub(_collections_abc.Sequence):
    """Sequence with only __getitem__/__len__: index/count/__contains__/
    __iter__/__reversed__ are mixins."""

    def __init__(self, items):
        self._items = list(items)

    def __getitem__(self, index):
        return self._items[index]

    def __len__(self):
        return len(self._items)


s = SeqStub("abcabc")
print("seq mro", [c.__name__ for c in type(s).__mro__])
print("seq index", s.index("b"), s.index("c", 3))
print("seq count", s.count("a"), s.count("z"))
print("seq contains", "c" in s, "z" in s)
print("seq iter", list(iter(s)))
print("seq reversed", list(reversed(s)))
try:
    s.index("z")
except ValueError as exc:
    print("seq index miss ValueError", exc)


class SetStub(_collections_abc.MutableSet):
    """MutableSet with the 3 abstract stubs: |= and friends are mixins."""

    def __init__(self, items=()):
        self._s = set(items)

    def __contains__(self, item):
        return item in self._s

    def __iter__(self):
        return iter(self._s)

    def __len__(self):
        return len(self._s)

    def add(self, item):
        self._s.add(item)

    def discard(self, item):
        self._s.discard(item)


t = SetStub("ab")
print("set mro", [c.__name__ for c in type(t).__mro__])
t |= SetStub("bc")
print("set ior", sorted(t), type(t).__name__)
t -= SetStub("a")
print("set isub", sorted(t))
u = t | SetStub("xy")
print("set or", sorted(u), type(u).__name__)
print("set isdisjoint", t.isdisjoint(SetStub("z")), t.isdisjoint(SetStub("b")))
t.remove("b")
print("set remove", sorted(t))
try:
    t.remove("gone")
except KeyError as exc:
    print("set remove miss KeyError", exc)
