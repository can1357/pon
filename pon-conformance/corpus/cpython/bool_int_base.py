# Derived from CPython v3.14.0 Lib/test/test_bool.py topics (PSF license).
#
# `bool` linearizes through `int`: the type-graph surface (`__mro__`,
# `__bases__`, issubclass/isinstance matrix), arithmetic identity,
# sequence-index parity, and the dict/set key domain collapse.

# --- type graph --------------------------------------------------------------
print(bool.__mro__)
print(bool.__bases__)
print(int.__mro__)
print(int.__bases__)

# --- issubclass matrix -------------------------------------------------------
print(issubclass(bool, int), issubclass(bool, object), issubclass(bool, bool))
print(issubclass(int, bool), issubclass(int, object))
print(issubclass(bool, (str, int)), issubclass(bool, (str, float)))

# --- isinstance matrix -------------------------------------------------------
print(isinstance(True, int), isinstance(False, int))
print(isinstance(True, bool), isinstance(False, bool))
print(isinstance(True, object), isinstance(True, float))
print(isinstance(1, bool), isinstance(0, bool))
print(isinstance(True, (str, int)), isinstance(False, (str, float)))

# --- constructor stays bool's own (never int.__new__ through the MRO) --------
print(bool(10), bool(-1), bool(0), bool("hello"), bool(""), bool())
print(int(True), int(False), type(int(True)) is int)

# --- arithmetic identity -----------------------------------------------------
print(True + True)
print(True == 1, False == 0, True == 1.0)
print(hash(True) == hash(1), hash(False) == hash(0))
print(True * 7, True - False, divmod(True, True))

# --- sequence index parity ---------------------------------------------------
values = [10, 20, 30]
print(values[True], values[False])
print((10, 20)[True], "python"[True], "python"[False])
print(values[False:True + True], "python"[False:True + 2])
print(list(range(False, True + True)))
print("abc".find("b") == True)

# --- dict-key collapse: 1 / True / 1.0 share one slot ------------------------
sentinel = ["payload"]
d = {1: sentinel}
print(d[True] is d[1], d[True] is sentinel)
d[True] = "replaced"
print(d, len(d))
e = {True: "t"}
e[1] = "i"
print(e, len(e))
print(True in {1: "x"}, 1 in {True: "x"}, 1.0 in {True: "x"})
print({True, 1, 1.0}, len({True, 1, 1.0}))
print(sorted([True, False, 2]))
