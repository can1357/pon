# User __hash__/__eq__ dispatch in hash() and the dict/set key domain:
# fixed user hashes, __eq__-driven key dedup, the class-creation
# __eq__-without-__hash__ unhashable rule, inherited hooks, identity-default
# preservation, and set membership through user hooks.  Dict/set LITERALS
# with user-hash keys exercise the constructor helpers directly.

# Fixed user hash: hash(obj) honors the hook exactly (CPython slot_tp_hash
# preserves in-range results as-is).
class FortyTwo:
    def __hash__(self):
        return 42

ft = FortyTwo()
print(hash(ft), hash(ft) == hash(42))

# Equal-hash keys WITHOUT __eq__ stay distinct dict keys (identity equality):
# collision probing must separate them from each other and from int 42.
f1 = FortyTwo()
f2 = FortyTwo()
collide = {f1: 'one', f2: 'two', 42: 'int'}
print(len(collide), collide[f1], collide[f2], collide[42])
print(f1 in collide, FortyTwo() in collide)

# __eq__-based key dedup: value-equal keys share one slot; first key wins,
# last value wins — including inside a dict literal (constructor path).
class Point:
    def __init__(self, x, y):
        self.x = x
        self.y = y
    def __hash__(self):
        return hash((self.x, self.y))
    def __eq__(self, other):
        return isinstance(other, Point) and (self.x, self.y) == (other.x, other.y)
    def __repr__(self):
        return f'Point({self.x}, {self.y})'

d = {Point(1, 2): 'a', Point(1, 2): 'b', Point(3, 4): 'c'}
print(len(d), d[Point(1, 2)], d[Point(3, 4)])
d[Point(1, 2)] = 'z'
print(len(d), d[Point(1, 2)])
print(Point(1, 2) in d, Point(9, 9) in d)
print(d.get(Point(3, 4)), d.get(Point(9, 9), 'dflt'))
print(d.setdefault(Point(1, 2), 'ignored'), d.setdefault(Point(5, 6), 'new'), len(d))
print(d.pop(Point(5, 6)), len(d))
del d[Point(3, 4)]
print(len(d), sorted(v for v in d.values()))

# update() dedups across dicts through user equality.
other = {Point(1, 2): 'updated', Point(7, 7): 'seven'}
d.update(other)
print(len(d), d[Point(1, 2)], d[Point(7, 7)])

# __eq__ without __hash__: class creation stamps __hash__ = None ->
# unhashable everywhere (hash(), dict key insert AND lookup, set element).
class EqOnly:
    def __init__(self, v):
        self.v = v
    def __eq__(self, other):
        return isinstance(other, EqOnly) and self.v == other.v

try:
    hash(EqOnly(1))
except TypeError as exc:
    print('hash:', exc)
try:
    {EqOnly(1): 'x'}
except TypeError as exc:
    print('key:', exc)
try:
    {} [EqOnly(1)]
except TypeError as exc:
    print('lookup:', exc)
try:
    {EqOnly(1)}
except TypeError as exc:
    print('elem:', exc)
print(EqOnly(1) == EqOnly(1), EqOnly(1) == EqOnly(2), EqOnly(1) != EqOnly(2))

# Explicit __hash__ = None is the same marker.
class Banned:
    __hash__ = None

try:
    hash(Banned())
except TypeError as exc:
    print('banned:', exc)

# Inherited hooks: subclasses share the base's hash/eq (same key domain);
# defining __hash__ in a subclass of an __eq__-only class re-enables hashing.
class SubPoint(Point):
    pass

sp = SubPoint(1, 2)
print(hash(sp) == hash(Point(1, 2)), sp == Point(1, 2))
print(d[sp])

class Rehashed(EqOnly):
    def __hash__(self):
        return 7

print(hash(Rehashed(1)), {Rehashed(1): 'ok'}[Rehashed(1)])

class StillBanned(EqOnly):
    pass

try:
    hash(StillBanned(1))
except TypeError as exc:
    print('inherited-none:', exc)

# Identity default preserved: plain objects keep working as dict keys and
# hash stably without any user hook.
class Plain:
    pass

p = Plain()
q = Plain()
ident = {p: 'p', q: 'q'}
print(len(ident), ident[p], ident[q], hash(p) == hash(p), p in ident, Plain() in ident)

# Set membership through user hooks: literals dedup, add/discard/remove
# dispatch equality, frozensets probe the same domain.
s = {Point(1, 2), Point(1, 2), Point(3, 4)}
print(len(s), Point(1, 2) in s, Point(9, 9) in s)
s.add(Point(1, 2))
print(len(s))
s.add(Point(5, 6))
print(len(s), Point(5, 6) in s)
s.discard(Point(5, 6))
print(len(s))
s.remove(Point(3, 4))
print(len(s))
try:
    s.remove(Point(3, 4))
except KeyError as exc:
    print('removed:', type(exc).__name__)

fs = frozenset([Point(1, 2), Point(1, 2), Point(8, 8)])
print(len(fs), Point(8, 8) in fs, Point(1, 2) in fs, Point(0, 0) in fs)
print(hash(fs) == hash(frozenset([Point(8, 8), Point(1, 2)])))

# User keys nested in tuple keys: hashing recurses, equality re-dispatches.
nest = {(Point(1, 2), 'k'): 'nested'}
print(nest[(Point(1, 2), 'k')])

# __hash__ result conversion: bools count as ints, huge values reduce like
# CPython's long hash only when out of Py_hash_t range, and -1 maps to -2.
class BoolHash:
    def __hash__(self):
        return True

class HugeHash:
    def __hash__(self):
        return 2 ** 64 + 3

class NegOne:
    def __hash__(self):
        return -1

print(hash(BoolHash()) == 1, hash(HugeHash()) == hash(2 ** 64 + 3), hash(NegOne()) == -2)

class BadHash:
    def __hash__(self):
        return 'text'

try:
    hash(BadHash())
except TypeError as exc:
    print('bad:', exc)

# Raising hooks propagate as themselves through hash() and container paths.
class Explosive:
    def __hash__(self):
        raise ValueError('kaboom')

try:
    hash(Explosive())
except ValueError as exc:
    print('raise:', exc)
try:
    {Explosive(): 1}
except ValueError as exc:
    print('raise-key:', exc)

# Cross-type equality claims via user __eq__ merge keys with builtins.
class IntLike:
    def __init__(self, v):
        self.v = v
    def __hash__(self):
        return hash(self.v)
    def __eq__(self, other):
        if isinstance(other, IntLike):
            return self.v == other.v
        return self.v == other

il = IntLike(5)
print(il == 5, 5 == il, hash(il) == hash(5))
merged = {5: 'five'}
print(merged[il], il in merged)
merged[il] = 'still five'
print(len(merged), merged[5])
print(len({IntLike(3), 3}), len({3, IntLike(3)}))
