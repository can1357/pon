# Keyword binding for functions that historically lowered through the
# record-less Phase-A shape (plain positional signatures, no defaults):
# __slots__ class __init__, eval/exec-compiled functions, and namedtuple's
# eval'd __new__ must all bind keywords through their parameter-name table.


class Slotted:
    __slots__ = ('x', 'y')

    def __init__(self, x, y):
        self.x = x
        self.y = y

    def move(self, dx, dy):
        return Slotted(self.x + dx, self.y + dy)


s = Slotted(x=1, y=2)
print("slots_kw", s.x, s.y)
s2 = Slotted(3, y=4)
print("slots_mixed", s2.x, s2.y)
m = s.move(dy=10, dx=20)
print("slots_method_kw", m.x, m.y)


class Plain:
    def __init__(self, a, b):
        self.a = a
        self.b = b


p = Plain(b=2, a=1)
print("plain_kw", p.a, p.b)


f = eval('lambda a, b=2: (a, b)')
print("eval_lambda_kw", f(a=1))
print("eval_lambda_both", f(b=9, a=8))

g = eval('lambda u, v: u - v')
print("eval_lambda_nodefault", g(v=3, u=10))

exec('def h(p, q=3):\n    return (p, q)')
print("exec_def_kw", h(q=7, p=4))

exec('def k(m, n):\n    return m * n')
print("exec_def_nodefault", k(n=6, m=7))


from collections import namedtuple

P = namedtuple('P', ['x', 'y'])
pt = P(x=5, y=6)
print("namedtuple_kw", pt)
print("namedtuple_mixed", P(11, y=12))
print("namedtuple_replace", pt._replace(x=9))
print("namedtuple_replace_all", pt._replace(x=0, y=1))
print("namedtuple_fields", P._fields)
