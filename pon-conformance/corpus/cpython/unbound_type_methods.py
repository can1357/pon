# CPython method-descriptor access on builtin TYPES: `int.bit_length` /
# `str.upper` / `list.append` reached off the type are unbound callables
# taking the instance as the first argument (vendored `_pydecimal` stores
# `_nbits = int.bit_length` at module scope), with the CPython 3.14
# method_descriptor error shapes for misuse.

def shape(fn):
    try:
        return repr(fn())
    except TypeError as e:
        return f"TypeError | {e}"

# --- int: unbound calls ------------------------------------------------------
print(int.bit_length(7))
print(int.bit_length(0), int.bit_length(-255))
print(int.bit_count(7))
print(int.to_bytes(258, 2, 'big'))
print(int.__index__(5), int.__format__(255, 'x'))
print(bool.bit_length(True))            # bool inherits through the int rung
print(bool.bit_length is int.bit_length)
print(int.bit_length(True))             # bool receiver accepted (bool <: int)
_nbits = int.bit_length                 # the _pydecimal module-scope pattern
print(_nbits(1023), _nbits(1024))

# --- int: error shapes -------------------------------------------------------
print(shape(lambda: int.bit_length()))
print(shape(lambda: int.bit_length('x')))
print(shape(lambda: int.bit_length(7, 8)))
print(shape(lambda: (7).bit_length(8)))
print(shape(lambda: int.to_bytes()))
print(shape(lambda: int.to_bytes('x', 1, 'big')))
print(shape(lambda: int.bit_count([])))

# --- str: unbound calls ------------------------------------------------------
print(str.upper('a'))
print(str.lower('AbC'), str.strip('  x  '))
up = str.upper
print(up('mixed'))

# --- str: error shapes -------------------------------------------------------
print(shape(lambda: str.upper()))
print(shape(lambda: str.upper(1)))
print(shape(lambda: str.upper(b'a')))
print(shape(lambda: str.upper('ab', 1)))

# --- list: unbound matrix ----------------------------------------------------
items = [1, 2]
print(list.append(items, 3), items)
list.extend(items, (4, 5))
print(items)
print(list.pop(items))
print(list.index(items, 2))
print(shape(lambda: list.append()))
print(shape(lambda: list.append((), 2)))
print(shape(lambda: list.append([])))

# --- callable survives storage and passing -----------------------------------
table = {'nbits': int.bit_length, 'up': str.upper}
print(table['nbits'](63), table['up']('q'))
print(list(map(int.bit_length, [1, 2, 4, 8])))
