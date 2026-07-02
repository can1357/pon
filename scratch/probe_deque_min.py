import _collections
print('imported')
deque = _collections.deque
d = deque([1, 2, 3])
print('constructed', len(d))
print('repr', repr(d))
print('bool', bool(d))
it = iter(d)
print('iter obtained')
print('next', next(it))
print('list', list(d))
for x in d:
    print('for', x)
