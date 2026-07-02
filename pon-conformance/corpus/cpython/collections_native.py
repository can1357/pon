import _collections
from _collections import defaultdict, deque

# --- deque construction and repr -------------------------------------------
print(repr(deque()))
print(repr(deque([1, "a", (2, 3)])))
print(repr(deque("abc")))
print(repr(deque([1, 2], maxlen=7)))
print(len(deque()), bool(deque()), len(deque([1])), bool(deque([1])))

# --- append / appendleft / maxlen overflow on both ends ---------------------
d = deque([1, 2, 3], maxlen=3)
d.append(4)
print(list(d), d.maxlen)
d.appendleft(0)
print(list(d))
d.extend([7, 8])
print(list(d))
d.extendleft([9, 10])
print(list(d))
z = deque(maxlen=0)
z.append(1)
z.appendleft(2)
print(list(z), len(z), bool(z), z.maxlen)
print(deque([1]).maxlen)

# --- extendleft reverses, iteration order ------------------------------------
e = deque()
e.extendleft([1, 2, 3])
print(list(e))
order = []
for item in deque(["x", "y", "z"]):
    order.append(item)
print(order)
print([2 * v for v in deque([1, 2, 3])])

# --- rotate: positive, negative, wrapping, default, empty --------------------
r = deque([1, 2, 3, 4, 5])
r.rotate(2)
print(list(r))
r.rotate(-2)
print(list(r))
r.rotate(7)
print(list(r))
r.rotate()
print(list(r))
empty = deque()
empty.rotate(3)
print(list(empty))

# --- pop / popleft / clear / copy -------------------------------------------
p = deque([1, 2, 3])
print(p.pop(), p.popleft(), list(p))
try:
    deque().pop()
except IndexError as exc:
    print("IndexError:", exc)
try:
    deque().popleft()
except IndexError as exc:
    print("IndexError:", exc)
o = deque([1, 2], maxlen=4)
q = o.copy()
q.append(3)
print(list(o), list(q), q.maxlen)
o.clear()
print(list(o), len(o))

# --- count / remove / contains / index ---------------------------------------
c = deque([1, 2, 1, 3, 1])
print(c.count(1), c.count(9))
c.remove(1)
print(list(c))
try:
    c.remove(99)
except ValueError as exc:
    print("ValueError:", exc)
print(2 in c, 99 in c, "a" in deque("abc"))
i = deque(["a", "b", "c", "b"])
print(i.index("b"), i.index("b", 2), i.index("c", -3), i.index("b", 1, 2))
try:
    i.index("nope")
except ValueError as exc:
    print("ValueError:", exc)

# --- equality: content-based, maxlen-blind, non-deque operands ----------------
print(deque([1, 2]) == deque([1, 2]), deque([1, 2]) == deque([1, 2], maxlen=5))
print(deque([1, 2]) == deque([2, 1]), deque([1, 2]) == [1, 2], deque() == deque())
print(deque([1, 2]) != deque([1, 3]), deque([1, 2]) != deque([1, 2]))

# --- defaultdict: int / list / lambda factories ------------------------------
di = defaultdict(int)
print(di["x"], len(di), sorted(di.keys()))
di["x"] += 5
print(di["x"], di == {"x": 5}, {"x": 5} == di)
dl = defaultdict(list)
dl["a"].append(1)
dl["a"].append(2)
dl["b"].append(3)
print(sorted(dl.items()))
lam = defaultdict(lambda: "missing")
print(lam["k"], "k" in lam, len(lam))

# --- miss-insert visibility ---------------------------------------------------
vis = defaultdict(int)
print("v" in vis, len(vis))
_ = vis["v"]
print("v" in vis, len(vis), vis["v"], sorted(vis.items()))

# --- no factory / None factory: KeyError typed and catchable ------------------
nf = defaultdict()
print(nf.default_factory)
try:
    nf["q"]
except KeyError as exc:
    print("KeyError:", exc)
print(len(nf), "q" in nf)
nn = defaultdict(None)
try:
    nn["q"]
except KeyError as exc:
    print("KeyError:", exc)

# --- get / in / keys never trigger the factory --------------------------------
g = defaultdict(int)
print(g.get("z"), g.get("z", 9), "z" in g, len(g), sorted(g.keys()))

# --- factory attribute readable and writable ----------------------------------
print(di.default_factory is int, dl.default_factory is list)
di.default_factory = list
print(di.default_factory is list)

# --- init with mapping, repr ---------------------------------------------------
m = defaultdict(int, {"a": 1, "b": 2})
print(sorted(m.items()), m["c"], sorted(m.items()))
print(repr(defaultdict(int, {"a": 1})))
print(repr(defaultdict(list)))
print(repr(defaultdict()))

# --- non-callable factory ------------------------------------------------------
try:
    defaultdict(42)
except TypeError as exc:
    print("TypeError:", exc)

# --- nested defaultdict(list).append pattern -----------------------------------
tree = defaultdict(list)
for key, value in [("a", 1), ("b", 2), ("a", 3)]:
    tree[key].append(value)
print(sorted(tree.items()))
nested = defaultdict(lambda: defaultdict(list))
nested["outer"]["inner"].append(1)
print(sorted((k, sorted(v.items())) for k, v in nested.items()))

# --- dict-protocol surface on defaultdict --------------------------------------
proto = defaultdict(int, {"k": 1})
proto["j"] = 2
print(sorted(proto.items()), len(proto), sorted(proto.values()))
print(isinstance(proto, dict), proto.pop("j"), sorted(proto.items()))

# --- collections module identity with _collections ------------------------------
import collections

print(collections.deque is _collections.deque)
print(collections.defaultdict is _collections.defaultdict)
print(collections.deque is deque, collections.defaultdict is defaultdict)
print(type(deque([1])).__name__, type(defaultdict()).__name__)
cd = collections.deque("ab", maxlen=2)
cd.append("c")
print(list(cd))
cdd = collections.defaultdict(list)
cdd["z"].append(1)
print(sorted(cdd.items()))
