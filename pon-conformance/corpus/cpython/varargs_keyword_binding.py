# Varargs and keyword binding safety shapes.


def f(a, b=0, *args, **kw):
    return a, b, args, sorted(kw.items())


def g(a, *args):
    return a, args


def no_varargs(a):
    return a


print("f keyword", f(1, b=2))
print("f default", f(1))
print("f mixed", f(1, 2, 3, 4, x=5))
print("g positional", g(1))
print("g keyword", g(a=1))

try:
    no_varargs(1, 2)
except TypeError as exc:
    print("too many", type(exc).__name__)
