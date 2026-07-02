# sys.version_info is a structseq: a tuple subclass with named read-only
# fields (major, minor, micro, releaselevel, serial), so indexing, slicing,
# attribute reads, tuple comparison, and f-string consumption all resolve the
# same elements.  The vendored stdlib depends on the tuple shape at import
# time (sysconfig builds f"{sys.version_info[0]}.{sys.version_info[1]}"; a
# plain-string stand-in silently produced 's.y' paths).  micro/releaselevel/
# serial are printed through the object itself so the differential tracks the
# host oracle without pinning a patch level here.

import sys

v = sys.version_info

# Structseq shape.
print(type(v).__name__, type(v).__module__)
print(isinstance(v, tuple), type(v) is tuple, len(v))

# Indexing and attribute access resolve the same elements.
print(v[0], v[1])
print(v.major, v.minor)
print(v.major == v[0], v.minor == v[1], v.micro == v[2])
print(v.releaselevel == v[3], v.serial == v[4])
print(v[2] >= 0, v[3] in ('alpha', 'beta', 'candidate', 'final'), v[4] >= 0)
print(v[-1] == v.serial, v[-5] == v.major)

# Slicing returns plain tuples.
print(v[:2], type(v[:2]) is tuple)
print(v[::2] == (v[0], v[2], v[4]), v[3:] == (v.releaselevel, v.serial))

# Comparison with plain tuples, both directions.
print(v >= (3,), v >= (3, 10), (4, 0) > v, v < (4,))
print(v == tuple(v), tuple(v) == v, v != (0, 0))
print((v.major, v.minor) == v[:2])

# The sysconfig path shapes.
print(f'python{v[0]}.{v[1]}')
print('lib/python%d.%d' % v[:2])
print('{}.{}'.format(*v[:2]))

# Iteration, unpacking, conversion, hash parity with the equal plain tuple.
major, minor, micro, releaselevel, serial = v
print(major == v.major, minor == v.minor, serial == v.serial)
print(list(v) == [v[0], v[1], v[2], v[3], v[4]])
print(v.index(v.minor) <= 1, v.count(v.releaselevel) >= 1)
print(hash(v) == hash(tuple(v)))
print({v: 'here'}[tuple(v)])

# repr is the structseq form; str matches it.
print(str(v) == repr(v))
print(repr(v).startswith('sys.version_info(major='))
