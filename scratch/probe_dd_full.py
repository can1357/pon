import _collections
from _collections import defaultdict, deque

# defaultdict basics
d = defaultdict(int)
print(d['x'], d['x'] + 1, len(d), sorted(d.keys()))
d['x'] += 5
print(d['x'], d == {'x': 5})

dl = defaultdict(list)
dl['a'].append(1)
dl['a'].append(2)
dl['b'].append(3)
print(sorted(dl.items()))

lam = defaultdict(lambda: 'missing')
print(lam['k'], 'k' in lam, len(lam))

# no factory
nf = defaultdict()
print(nf.default_factory)
try:
    nf['q']
except KeyError as exc:
    print('KeyError caught', exc)
print(len(nf), 'q' in nf)

# None factory explicit
nn = defaultdict(None)
try:
    nn['q']
except KeyError as exc:
    print('KeyError caught 2', exc)

# get/in never trigger factory
g = defaultdict(int)
print(g.get('z'), g.get('z', 9), 'z' in g, len(g))

# factory visible + writable
print(d.default_factory is int, dl.default_factory is list)
d.default_factory = list
print(d.default_factory is list)

# init with mapping
m = defaultdict(int, {'a': 1, 'b': 2})
print(sorted(m.items()), m['c'], sorted(m.items()))

# repr
print(repr(defaultdict(int, {'a': 1})))
print(repr(defaultdict(list)))
print(repr(defaultdict()))

# non-callable factory
try:
    defaultdict(42)
except TypeError as exc:
    print('TypeError:', exc)

# nested defaultdict(list) pattern
tree = defaultdict(list)
for k, v in [('a', 1), ('b', 2), ('a', 3)]:
    tree[k].append(v)
print(sorted(tree.items()))

# identity with collections
import collections
print(collections.defaultdict is defaultdict, collections.deque is deque)
print(isinstance(d, dict))
