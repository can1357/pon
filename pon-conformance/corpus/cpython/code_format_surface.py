# code.replace + int-subclass __format__ surface: deterministic reads only.
#
# (1) int.__format__ is a real method on the int type, so int SUBCLASS
# instances format through the int path: enum copies member_type.__format__
# into IntEnum/IntFlag class dicts (enum.py:573-575) and plain int
# subclasses resolve it through the MRO ahead of object.__format__.
# type(int.__format__).__name__ is NOT asserted (method_descriptor in
# CPython vs. pon's plain-function carrier).  Error assertions are
# type-only: clinic message texts are not byte-matched.
#
# (2) code.replace(**co_kwargs) returns a NEW code object with the named
# co_* fields swapped and the receiver untouched; pon's code shells carry
# metadata only (execution never reads them), so the swapped attribute
# surface IS the observable contract, and the vendored types.coroutine
# (types.py:311, the test.support import-time consumer) runs end to end.
# Raw co_flags VALUES are not printed (bit inventories differ across
# implementations); assertions are relational.  compile()-produced code
# objects are a different pon shell and are deliberately not probed.
import enum
import types


class Color(enum.IntEnum):
    RED = 1
    GREEN = 2


class Perm(enum.IntFlag):
    R = 4
    W = 2


class MyInt(int):
    pass


# --- int-subclass __format__ through the int path ---
print(format(Color.RED, '03d'))
print(format(Color.GREEN, 'x'), format(Color.GREEN, '#06b'))
print(format(Perm.R, 'd'), format(Perm.W, '<4d') + '|')
print(format(True, 'd'), format(False, '05d'))
print(format(True), format(False))
print(format(MyInt(255), '#x'), format(MyInt(-42), '=+10d') + '|')
print(format(MyInt(1234567), ',d'))
print(Color.RED.__format__('05d'))
print(f"{Color.RED:03d}|{Perm.W:b}|{MyInt(7):+d}")
print(format(Color.RED), format(Perm.R, ''))
try:
    format(Color.RED, 's')
except ValueError as exc:
    print('ValueError', type(exc).__name__)

# --- code.replace ---
def gen():
    yield 1


co = gen.__code__
print(type(co).__name__)
clone = co.replace()
print(clone is co, type(clone).__name__, clone.co_name)
flagged = co.replace(co_flags=co.co_flags | 0x100)
print(flagged.co_flags == co.co_flags | 0x100, co.co_flags & 0x100)
renamed = flagged.replace(co_name='renamed', co_filename='cleaned.py')
print(renamed.co_name, renamed.co_filename, renamed.co_flags == flagged.co_flags)
print(co.co_name, co.co_filename != 'cleaned.py')
try:
    co.replace(bogus=1)
except TypeError as exc:
    print('TypeError', 'unexpected keyword' in str(exc))
try:
    co.replace(co_flags='x')
except TypeError as exc:
    print('TypeError')
try:
    co.replace(1)
except TypeError as exc:
    print('TypeError', exc)

# --- __code__ assignment round-trip (test.support reset_code shape) ---
gen.__code__ = gen.__code__.replace()
print(list(gen()))
try:
    gen.__code__ = 5
except TypeError as exc:
    print('TypeError', exc)

# --- the vendored import-time consumer: types.coroutine (types.py:311) ---
@types.coroutine
def async_yield(v):
    return (yield v)


print(async_yield.__code__.co_flags & 0x100 == 0x100)
print(list(async_yield('x')))
