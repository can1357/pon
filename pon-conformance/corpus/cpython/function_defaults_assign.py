# Live reassignment of function __defaults__ / __kwdefaults__ after creation
# (CPython func_set_defaults / func_set_kwdefaults rules): assignment replaces
# the whole tuple/dict, None and deletion clear entirely, non-tuple/non-dict
# assignment raises TypeError, and the binder consults the live value at every
# call.


# Assignment after creation is honored at call time (function had no
# creation-time defaults at all).
def f(a, b, c):
    return (a, b, c)


f.__defaults__ = (2, 3)
print(f(1))
print(f(1, 9))
print(f(1, 9, 8))
print(f.__defaults__)


# Replacement is wholesale, never a merge with creation-time defaults.
def g(a, b=10, c=20):
    return (a, b, c)


print(g(1))
g.__defaults__ = (99,)
print(g(1, 2))
print(g.__defaults__)


# Assigning None clears the defaults; reads report None.
def h(a, b=5):
    return (a, b)


h.__defaults__ = None
print(h.__defaults__)
print(h(1, 2))


# Deletion clears exactly like assigning None.
def h2(a, b=5):
    return (a, b)


del h2.__defaults__
print(h2.__defaults__)


# An empty tuple also leaves no defaults at call time but reads back as ().
def h3(a, b=5):
    return (a, b)


h3.__defaults__ = ()
print(h3.__defaults__)
print(h3(1, 2))


# Over-long defaults tuple: tail alignment — the last len(params) entries
# cover the parameters, the head is unused.
def k(a, b, c):
    return (a, b, c)


k.__defaults__ = (9, 8, 7, 6, 5)
print(k())
print(k(1))
print(k(1, 2))
print(k.__defaults__)


# Non-tuple assignment raises CPython's TypeError; tuple identity of the
# stored object is preserved on read.
def m(a):
    return a


try:
    m.__defaults__ = [1, 2]
except TypeError as exc:
    print(exc)
try:
    m.__defaults__ = 5
except TypeError as exc:
    print(exc)
t = (1, 2)
m.__defaults__ = t
print(m.__defaults__ is t)


# __kwdefaults__ replacement drives keyword-only binding the same way.
def kw(a, *, flag=False, depth=1):
    return (a, flag, depth)


print(kw(1))
kw.__kwdefaults__ = {"flag": True, "depth": 3}
print(kw(1))
print(kw(1, depth=9))
print(kw.__kwdefaults__)
kw.__kwdefaults__ = None
print(kw.__kwdefaults__)
kw.__kwdefaults__ = {"flag": None, "depth": 0}
print(kw(1))
try:
    kw.__kwdefaults__ = [1]
except TypeError as exc:
    print(exc)


# Mixed: positional defaults replaced while keyword-only defaults stay from
# creation time.
def mixed(a, b=1, *, tag="x"):
    return (a, b, tag)


print(mixed(0))
mixed.__defaults__ = (42,)
print(mixed(0))
print(mixed(0, tag="y"))


# Overrides reach bound-method calls (receiver prepended) and */** call
# shapes through the same binder.
class C:
    def m(self, a, b):
        return (a, b)


C.m.__defaults__ = (7,)
c = C()
print(c.m(1))
print(c.m(1, 2))
print(C.m.__defaults__)


def star_target(a, b, c):
    return (a, b, c)


star_target.__defaults__ = (30,)
print(star_target(*(1, 2)))


# namedtuple round-trip: vendored collections exec-compiles __new__ and then
# assigns __new__.__defaults__ — the binder must honor it so short
# construction fills the trailing fields from the live defaults.
from collections import namedtuple

P = namedtuple("P", "x y", defaults=(0,))
print(P.__new__.__defaults__)
p = P(1)
print(p)
print(p.x, p.y)
q = P(1, 2)
print(q)
print(tuple(q))
print(P(*p) == p)


# CPython validates with PyTuple_Check / PyDict_Check: SUBCLASS payloads are
# accepted, stored by identity, and drive binding from their storage.
Pair = namedtuple("Pair", "a b")


def sub_f(x, y, z):
    return (x, y, z)


sub_f.__defaults__ = Pair(20, 30)
print(sub_f(1))
print(sub_f(1, 2))
print(sub_f.__defaults__)
print(type(sub_f.__defaults__).__name__)


class D(dict):
    pass


def sub_kw(a, *, k=0):
    return (a, k)


d = D()
d["k"] = 9
sub_kw.__kwdefaults__ = d
print(sub_kw(1))
print(sub_kw(1, k=2))
print(type(sub_kw.__kwdefaults__).__name__)
