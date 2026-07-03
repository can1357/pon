import collections
import typing


class TP(typing.NamedTuple):
    a: int
    b: str
    c: int = 9


assert TP(1, 'z') == (1, 'z', 9)
assert TP(1, 'z').c == 9
assert TP(a=1, b='z', c=3).c == 3
assert TP._fields == ('a', 'b', 'c')
assert TP(1, 'z')._asdict() == {'a': 1, 'b': 'z', 'c': 9}


class ParsedRequirement(typing.NamedTuple):
    name: str
    url: str
    extras: tuple
    specifier: str
    marker: str


req = ParsedRequirement('demo', 'https://example.invalid/pkg', ('pdf', 'tls'), '>=1', 'python_version >= "3"')
assert req == ('demo', 'https://example.invalid/pkg', ('pdf', 'tls'), '>=1', 'python_version >= "3"')
assert req.name == 'demo'
assert req.extras == ('pdf', 'tls')
assert req[3] == '>=1'
assert ParsedRequirement._fields == ('name', 'url', 'extras', 'specifier', 'marker')

Point = collections.namedtuple('Point', 'x y')
p = Point(2, 3)
assert p == (2, 3)
assert p.x == 2
assert p.y == 3
assert p._asdict() == {'x': 2, 'y': 3}

print('ok')
