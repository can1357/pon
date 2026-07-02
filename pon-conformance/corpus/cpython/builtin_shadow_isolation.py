# Cross-module builtin-shadow isolation: a module-scope binding that shadows
# a builtin (reprlib's `repr = aRepr.repr`, enum's `class property(...)`)
# stays private to the module that bound it.  The importer keeps the real
# builtins; the shadow is visible only as the module's own attribute.

r0 = repr
p0 = property

print(len(repr('Z' * 40)))

import reprlib

print(len(repr('Z' * 40)))
print(repr is r0)
print(reprlib.repr is repr)
print(reprlib.repr('Z' * 40))
print(len(reprlib.repr('Z' * 40)))


def importer_probe():
    return len(repr('Z' * 40))


print(importer_probe())

import enum

print(property is p0)
print(enum.property is property)


class Color(enum.Enum):
    RED = 1
    GREEN = 2


print(Color.RED.name, Color.GREEN.value)


class Gauge:
    def __init__(self, level):
        self._level = level

    @property
    def level(self):
        return self._level * 10


print(Gauge(4).level)

# A module-scope shadow deleted with `del` re-exposes the builtin instead of
# evicting it process-wide.
abs = 'shadowed'
print(abs)
del abs
print(abs(-5))
