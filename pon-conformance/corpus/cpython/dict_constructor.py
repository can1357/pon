# dict() constructor forms and constructor-made dict behavior.
empty = dict()
print(empty, len(empty), bool(empty))

# Subscript insert/lookup round-trip with str/int/tuple/type keys.
d = dict()
d['a'] = 1
d[2] = 'two'
d[(3, 'k')] = [1, 2]
d[str] = 'S'
d[int] = 'I'
print(d)
print(d['a'], d[2], d[(3, 'k')], d[str], d[int])
print(len(d), bool(d))
print(list(d))
print(list(d.keys()))
print(list(d.values()))
print(list(d.items()))
for key in d:
    print('iter', key, '->', d[key])

# Mapping copy: constructor-made from literal and from constructor-made.
lit = {'x': 10, 'y': 20}
copy1 = dict(lit)
copy1['z'] = 30
print(copy1, lit)
copy2 = dict(copy1)
print(copy2)

# Iterable-of-pairs forms.
print(dict([('p', 1), ('q', 2)]))
print(dict([['a', 'b'], ('c', 'd')]))
print(dict(zip('ab', [10, 20])))
print(dict(['xy', 'zw']))
print(dict(()))

# fromkeys via the type and via an instance.
print(dict.fromkeys('abc'))
print(dict.fromkeys([1, 2], 'v'))
print(dict.fromkeys((), 'unused'))
print({}.fromkeys('ab', 3))

# get/setdefault on constructor-made dicts.
g = dict([('k', 'v')])
print(g.get('k'), g.get('nope'), g.get('nope', 'dflt'))
print(g.setdefault('k', 'other'), g.setdefault('fresh', 'minted'))
print(g)

# merge/update between constructor-made and literal dicts, both directions.
base = dict([('one', 1)])
base.update({'two': 2})
print(base)
target = {'zero': 0}
target.update(dict([('one', 1), ('zero', 100)]))
print(target)

# Unhashable keys raise CPython-shaped TypeError.
sink = dict()
try:
    sink[[1, 2]] = 'no'
except TypeError as exc:
    print('TypeError:', exc)
try:
    sink[{1: 2}] = 'no'
except TypeError as exc:
    print('TypeError:', exc)
try:
    sink[bytearray(b'ab')] = 'no'
except TypeError as exc:
    print('TypeError:', exc)
try:
    sink[(1, [2])] = 'no'
except TypeError as exc:
    print('TypeError:', exc)
try:
    print(sink[[3]])
except TypeError as exc:
    print('TypeError:', exc)
try:
    dict.fromkeys([[1]])
except TypeError as exc:
    print('TypeError:', exc)

# Malformed constructor arguments.
try:
    dict(42)
except TypeError as exc:
    print('TypeError:', exc)
try:
    dict([42])
except TypeError as exc:
    print('TypeError:', exc)
try:
    dict([(1, 2, 3)])
except ValueError as exc:
    print('ValueError:', exc)
try:
    dict('ab')
except ValueError as exc:
    print('ValueError:', exc)

# Constructor-made dicts share the literal dict type.
print(type(dict()) is dict, type({}) is dict)
print(isinstance(dict(), dict), isinstance({}, dict))
