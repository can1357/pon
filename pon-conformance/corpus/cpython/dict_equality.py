# Structural dict equality: ==/!= compare contents, ordering ops raise.
print({'a': 1} == {'a': 1})
print({'a': 1} == {'a': 2})
print({'a': 1} == {'b': 1})
print({'a': 1} == {'a': 1, 'b': 2})
print({'a': 1, 'b': 2} == {'b': 2, 'a': 1})

# Empty dicts.
print({} == {})
print({} != {})

# Constructor-made vs literal.
print(dict() == {})
print(dict([('p', 1), ('q', 2)]) == {'p': 1, 'q': 2})
print(dict(zip('ab', [1, 2])) == {'a': 1, 'b': 2})

# Nested dict values compare structurally.
print({'x': {'y': [1, 2]}} == {'x': {'y': [1, 2]}})
print({'x': {'y': 1}} == {'x': {'y': 2}})
print({'outer': {'inner': {'leaf': (1, 2)}}} == {'outer': {'inner': {'leaf': (1, 2)}}})

# Tuple keys.
print({(1, 2): 'v'} == {(1, 2): 'v'})
print({(1, 2): 'v'} == {(2, 1): 'v'})

# Numeric key/value cross-type equality.
print({1: 'a'} == {1.0: 'a'})
print({True: 'x'} == {1: 'x'})
print({'k': 1} == {'k': 1.0})
print({'k': 1} == {'k': True})

# Identical NaN value: identity implies equal.
nan = float('nan')
print({1: nan} == {1: nan})

# User-class values drive equality through __eq__.
class Tagged:
    def __init__(self, tag):
        self.tag = tag
    def __eq__(self, other):
        print('Tagged.__eq__', self.tag, other.tag)
        return isinstance(other, Tagged) and self.tag == other.tag
print({'k': Tagged(5)} == {'k': Tagged(5)})
print({'k': Tagged(5)} == {'k': Tagged(6)})
print({'k': Tagged(5)} != {'k': Tagged(5)})

# != both ways.
left = {'m': 1, 'n': 2}
right = {'n': 2, 'm': 1}
print(left != right, right != left)
other = {'m': 1, 'n': 3}
print(left != other, other != left)

# Cross-type comparisons are False, both ways.
print({'a': 1} == [('a', 1)])
print([('a', 1)] == {'a': 1})
print({'a': 1} != [('a', 1)])
print({} == None)
print({'a': 1} == 3)

# Self-referencing dicts terminate.
cyclic = {}
cyclic['self'] = cyclic
print(cyclic == cyclic)
shared = {'self': cyclic}
print(cyclic == shared)

# Ordering comparisons raise TypeError.
try:
    {} < {}
except TypeError as exc:
    print('TypeError:', exc)
try:
    {'a': 1} <= {'a': 1}
except TypeError as exc:
    print('TypeError:', exc)
try:
    {'a': 1} > {}
except TypeError as exc:
    print('TypeError:', exc)
try:
    {'a': 1} >= {'a': 1}
except TypeError as exc:
    print('TypeError:', exc)
try:
    {} < []
except TypeError as exc:
    print('TypeError:', exc)

# Dicts remain unhashable as dict keys.
try:
    {{'a': 1}: 'no'}
except TypeError as exc:
    print('TypeError:', exc)
