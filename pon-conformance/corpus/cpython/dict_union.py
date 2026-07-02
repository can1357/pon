# PEP 584 dict union: `|` builds a new dict, `|=` updates in place.

# Basic merge: right wins on conflicts, conflicting keys keep left position.
a = {'x': 1, 'y': 2}
b = {'y': 20, 'z': 30}
print(a | b)
print(b | a)
print(a, b)

# Empty-dict edges.
print({} | {})
print({} | {'k': 'v'})
print({'k': 'v'} | {})

# Union is shallow: values are shared, not copied.
inner = {'deep': 1}
print(({'n': inner} | {})['n'] is inner)

# `|=` with another dict: mutates in place, same object.
d = {'a': 1}
alias = d
d |= {'b': 2, 'a': 10}
print(d is alias, d)

# `|=` with iterables of key-value pairs: list, generator, zip.
d |= [('c', 3), ('a', 100)]
print(d is alias, d)
d |= (pair for pair in [('g', 7)])
print(d)
d |= zip('hi', (8, 9))
print(d)

# `|=` with a non-dict mapping (keys()/__getitem__ protocol).
class Mapping:
    def keys(self):
        return ('m1', 'm2')
    def __getitem__(self, key):
        return key.upper()
d |= Mapping()
print(d is alias, d)

# `|` requires dict operands on both sides.
try:
    {'a': 1} | [('b', 2)]
except TypeError as exc:
    print('TypeError:', exc)
try:
    [('b', 2)] | {'a': 1}
except TypeError as exc:
    print('TypeError:', exc)
try:
    {} | None
except TypeError as exc:
    print('TypeError:', exc)
try:
    {} | Mapping()
except TypeError as exc:
    print('TypeError:', exc)

# `|=` rejects non-iterables and malformed pair sequences.
try:
    d |= 5
except TypeError as exc:
    print('TypeError:', exc)
try:
    d |= [('one',)]
except ValueError as exc:
    print('ValueError:', exc)

# Subclass operands produce a plain dict for `|`.
class D(dict):
    pass
left = D()
left['a'] = 1
right = D()
right['a'] = 9
right['b'] = 2
u = left | right
print(type(u) is dict, u)
print(type(left | {'p': 0}) is dict)
print(type({'p': 0} | left) is dict)

# `|=` on a subclass keeps the subclass object.
s = D()
s['s'] = 1
salias = s
s |= {'t': 2}
print(s is salias, type(s) is D, dict(s))

# A user `__ror__` fires after dict's `__or__` declines the operand.
class Reflect:
    def __ror__(self, other):
        return ('ror', sorted(other))
print({'q': 1, 'p': 2} | Reflect())
