# CPython 3.14 unhashable wording: set paths wrap the bare message as
# "cannot use 'X' as a set element (unhashable type: 'X')".
try:
    {[1]}
except TypeError as exc:
    print("literal:", exc)

try:
    set().add([1])
except TypeError as exc:
    print("add:", exc)

try:
    {1}.add({})
except TypeError as exc:
    print("add dict:", exc)

try:
    [1] in {1, 2}
except TypeError as exc:
    print("contains:", exc)

try:
    [1] in frozenset({1, 2})
except TypeError as exc:
    print("frozenset contains:", exc)

try:
    set([[1]])
except TypeError as exc:
    print("set ctor:", exc)

try:
    frozenset([[1]])
except TypeError as exc:
    print("frozenset ctor:", exc)

# Contrast: dict keys use their own wording, hash() stays bare.
d = {}
try:
    d[[1]] = 1
except TypeError as exc:
    print("dict key:", exc)

try:
    hash([1])
except TypeError as exc:
    print("hash:", exc)
