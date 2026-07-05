# CPython Py_ReprEnter placeholders for self-referential containers and
# repr cycles through user __repr__ (numpy's meson FeatureObject graph),
# plus RecursionError enforcement at sys.getrecursionlimit().
import sys


class F:
    def __init__(self, name):
        self.name = name
        self.implies = set()

    def __hash__(self):
        return hash(self.name)

    def __eq__(self, other):
        return isinstance(other, F) and self.name == other.name

    def __repr__(self):
        return f'F({self.name}, {self.implies})'


a, b = F('a'), F('b')
a.implies = {b}
b.implies = {a}
print(repr(a))
print(repr(frozenset([a])))

l = [1]
l.append(l)
print(repr(l))
d = {}
d['k'] = d
print(repr(d))
t = ([],)
t[0].append(t)
print(repr(t))


def down(n):
    if n == 0:
        return 0
    return down(n - 1) + 1


print(down(400))
try:
    down(2000)
except RecursionError as exc:
    print('RecursionError', exc)
sys.setrecursionlimit(4000)
print(down(2500))
sys.setrecursionlimit(1000)
