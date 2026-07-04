def show(label, fn):
    try:
        print(label, "=>", fn())
    except Exception as e:
        print(label, "ERR", type(e).__name__, repr(str(e)))

import collections
Point = collections.namedtuple("Point", ["x", "y"])
show("collections.namedtuple new", lambda: Point(1, 2))
show("collections.namedtuple fields", lambda: (Point(1, 2).x, Point(1, 2).y))
show("collections.namedtuple kw", lambda: Point(x=1, y=2))

import typing
class TP(typing.NamedTuple):
    a: int
    b: str
    c: int = 9
show("typing.NamedTuple new", lambda: TP(1, "z"))
show("typing.NamedTuple default", lambda: TP(1, "z").c)
show("typing.NamedTuple kw", lambda: TP(a=1, b="z", c=3))
