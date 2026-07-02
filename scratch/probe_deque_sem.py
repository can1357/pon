from _collections import deque

# maxlen overflow: append drops from left, appendleft drops from right
d = deque([1, 2, 3], maxlen=3)
d.append(4)
print(list(d), d.maxlen)
d.appendleft(0)
print(list(d))
d.extend([7, 8])
print(list(d))
d.extendleft([9, 10])
print(list(d))

# maxlen=0
z = deque(maxlen=0)
z.append(1)
z.appendleft(2)
print(list(z), len(z), bool(z))

# rotate signs and wrap
r = deque([1, 2, 3, 4, 5])
r.rotate(2)
print(list(r))
r.rotate(-2)
print(list(r))
r.rotate(7)
print(list(r))
r.rotate()
print(list(r))
deque().rotate(3)

# extendleft reverses
e = deque()
e.extendleft([1, 2, 3])
print(list(e))

# pop/popleft + empty errors
p = deque([1, 2, 3])
print(p.pop(), p.popleft(), list(p))
try:
    deque().pop()
except IndexError as exc:
    print('IndexError:', exc)
try:
    deque().popleft()
except IndexError as exc:
    print('IndexError:', exc)

# count / remove / in / index
c = deque([1, 2, 1, 3, 1])
print(c.count(1), c.count(9))
c.remove(1)
print(list(c))
try:
    c.remove(99)
except ValueError as exc:
    print('ValueError:', exc)
print(2 in c, 99 in c)
i = deque(['a', 'b', 'c', 'b'])
print(i.index('b'), i.index('b', 2), i.index('c', -3), i.index('b', 1, 2))
try:
    i.index('zzz')
except ValueError as exc:
    print('ValueError:', exc)
try:
    i.index('b', 2, 3)
except ValueError as exc:
    print('ValueError:', exc)

# equality: content-based, maxlen ignored; vs non-deque False
print(deque([1, 2]) == deque([1, 2]), deque([1, 2]) == deque([1, 2], maxlen=5))
print(deque([1, 2]) == deque([2, 1]), deque([1, 2]) == [1, 2], deque() == deque())
print(deque([1, 2]) != deque([1, 3]))

# copy independence + clear
o = deque([1, 2], maxlen=4)
q = o.copy()
q.append(3)
print(list(o), list(q), q.maxlen)
o.clear()
print(list(o), len(o))

# iteration order and nesting
n = deque([deque([1]), deque([2])])
print([list(x) for x in n])

# repr
print(repr(deque()), repr(deque([1, 'a'])), repr(deque([1], maxlen=9)))

# constructor from string and empty-with-maxlen
print(list(deque('abc')), repr(deque('ab', maxlen=1)))
