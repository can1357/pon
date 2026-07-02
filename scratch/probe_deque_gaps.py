import _collections
deque = _collections.deque

d = deque([1, 2, 3])
print(d, len(d), bool(d), list(d))
print(type(d).__name__)
print(type(iter(d)).__name__)
d2 = deque([1, 2, 3])
try:
    print('eq', d == d2, d != d2, d == deque([1, 2]))
except Exception as exc:
    print('eq FAIL', type(exc).__name__, exc)
try:
    print('contains', 2 in d, 9 in d)
except Exception as exc:
    print('contains FAIL', type(exc).__name__, exc)
try:
    print('index', d.index(2))
except Exception as exc:
    print('index FAIL', type(exc).__name__, exc)
try:
    m = deque([1, 2, 3], maxlen=2)
    print('kw maxlen', list(m), m.maxlen)
except Exception as exc:
    print('kw maxlen FAIL', type(exc).__name__, exc)
try:
    m2 = deque([1, 2, 3], 2)
    print('pos maxlen', list(m2), m2.maxlen)
except Exception as exc:
    print('pos maxlen FAIL', type(exc).__name__, exc)
d.rotate(1)
print('rot+1', list(d))
d.rotate(-1)
print('rot-1', list(d))
print('repr', repr(deque([1], maxlen=3)))
