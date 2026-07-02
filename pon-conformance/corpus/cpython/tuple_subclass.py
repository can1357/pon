# Tuple-subclass instances embed native tuple storage (the namedtuple
# substrate): construction routes through tuple.__new__ (no __init__ leg
# re-runs it), and the full read-only protocol surface — len/iter/index/
# slice/eq/hash — must behave like CPython on the subclass layout.

class T(tuple):
    pass

t = T((1, 2, 3))
print(t)
print(len(t))
print(t[0], t[-1], t[1])
print(list(t))
print(t == (1, 2, 3), (1, 2, 3) == t)
print(t == T((1, 2, 3)), t != (1, 2, 4))
print(2 in t, 99 in t)
print(t[1:], t[:2], type(t[1:]) is tuple)
print(t.index(2), t.count(3), t.count(99))
print(t + (4, 5), (0,) + t)
print(t * 2)
print(type(t) is T, isinstance(t, tuple), isinstance(t, T))
print(str(t), repr(t), [t])

# Constructor forms.
print(T())
print(T("ab"))
print(T([5, 6])[1])
print(T(range(3)))
print(T(x * x for x in range(3)))
print(type(T("ab")) is T)

# Iteration protocols.
for i, v in enumerate(T(["p", "q"])):
    print(i, v)
a, b, c = t
print(a, b, c)
print(tuple(t), list(zip(t, "xyz")))
print(max(t), min(t), sum(t))

# Hash: equal contents key the same dict slot across both layouts.
d = {t: "sub"}
print(d[(1, 2, 3)])
d[(1, 2, 3)] = "exact"
print(d[t], len(d))
print(hash(T(())) == hash(()))
print(len({t, (1, 2, 3), T((1, 2, 3))}))

# Subclasses carry methods and instance attributes over the storage.
class Point(tuple):
    def __new__(cls, x, y):
        return super().__new__(cls, (x, y))

    def norm2(self):
        return self[0] * self[0] + self[1] * self[1]

p = Point(3, 4)
print(p, p.norm2(), len(p))
p.tag = "origin-ish"
print(p.tag, p == (3, 4))

# namedtuple: the pure-Python machinery over the substrate.
from collections import namedtuple

N = namedtuple("N", "a b")
n = N(1, 2)
print(n)
print(n._fields)
print(n.a, n.b, n[0], n[1])
print(n._asdict())
print(n._replace(b=20))
print(n._replace(a=10, b=20)._asdict())
print(N._make([7, 8]))
print(tuple(n), list(n), len(n))
x, y = n
print(x, y)
print(n == (1, 2), (1, 2) == n, n == N(1, 2), n != N(1, 3))
print(n.index(2), n.count(1))
print(isinstance(n, tuple), type(n) is N)

# namedtuple defaults (keyword construction is a binder-lane gap: the
# eval()-compiled __new__ lambda carries no Phase-B keyword metadata).
P = namedtuple("P", "x y", defaults=(0,))
print(P(1))
print(P(1, 2))
print(P._field_defaults)

# namedtuples as dict keys and set members.
score = {N(1, 2): "left", N(3, 4): "right"}
print(score[N(1, 2)], score[(3, 4)])
print(N(1, 2) in score, (1, 2) in score, N(9, 9) in score)

# Nested and mixed reprs.
pair = N(N(1, 2), (3, 4))
print(pair)
print(pair.a.b, pair[1][0])
print({"k": n}, [n, (1, 2)])
