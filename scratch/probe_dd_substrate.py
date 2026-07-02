class DD(dict):
    def __init__(self, factory=None):
        self.default_factory = factory

    def __missing__(self, key):
        if self.default_factory is None:
            raise KeyError(key)
        value = self.default_factory()
        self[key] = value
        return value


d = DD(int)
d['a'] = 1
print(d['a'], len(d), 'a' in d, d.get('b'), d.get('b', 7))
print(sorted(d.keys()), sorted(d.items()))
print(d == {'a': 1}, {'a': 1} == d, d != {'a': 2})
print(repr(d))
print(repr(int), repr(list))
print(isinstance(d, dict))
try:
    d['zz']
except KeyError as exc:
    print('KeyError', exc)
print('after-miss len', len(d), 'zz' in d)
print(dict.__repr__ is not None, DD.__init__ is not None)
