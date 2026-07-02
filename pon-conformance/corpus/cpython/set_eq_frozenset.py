# Set equality, ordering comparisons, and frozenset semantics.

a = {0, 1, 2, 3}
print(a == {3, 2, 1, 0})
print(a != {3, 2, 1, 0})
print(a == {0, 1})
print(a != {0, 1})
print({1, 2} <= a)
print({1, 2} < a)
print(a <= a)
print(a < a)
print(a >= {2, 3})
print(a > {2, 3})
print(a > a)
print({1, 9} <= a)
print(a == [0, 1, 2, 3])
print(a != "abc")
print(set() == set())
print(set() < {1})

# frozenset construction and basics
empty = frozenset()
print(empty)
print(len(empty))
fs = frozenset([3, 1, 2, 1])
print(len(fs))
print(sorted(fs))
print(2 in fs)
print(9 in fs)
print(fs.__contains__(3))
print(fs.__contains__("3"))

# iteration
total = 0
for item in fs:
    total = total + item
print(total)

# cross-type equality and ordering
print(fs == {1, 2, 3})
print({1, 2, 3} == fs)
print(fs != {1, 2})
print(fs == frozenset({1, 2}))
print(fs <= {1, 2, 3, 4})
print({1, 2} < fs)
print(fs >= frozenset([1]))
print(set() == frozenset())
print({9} == frozenset([9]))

# hashing and dict keys
print(hash(fs) == hash(frozenset([1, 2, 3])))
print(hash(frozenset([3, 2, 1])) == hash(fs))
print(hash(frozenset(["b", "a"])) == hash(frozenset(["a", "b"])))
print(hash(frozenset()) == hash(frozenset([])))
table = {fs: "digits", frozenset(): "empty"}
print(table[frozenset([1, 2, 3])])
print(table[frozenset()])
print(frozenset([1, 2, 3]) in table)
print(frozenset([1, 4]) in table)

# constructor pass-through and repr
print(frozenset(fs) is fs)
print(frozenset([7]))
print(repr(frozenset()))

# operators keep CPython result types
u = fs | frozenset([4])
print(type(u) is frozenset)
print(sorted(u))
i2 = fs & frozenset([2, 3, 9])
print(type(i2) is frozenset)
print(sorted(i2))
d = fs - frozenset([1])
print(type(d) is frozenset)
print(sorted(d))
m = {1, 2} | {3}
print(type(m) is set)
print(sorted(m))

# set() builtin interops with set literals
print(set([1, 2]) == {1, 2})
print({1, 2} == set([1, 2]))
print(sorted(set([2, 1, 2])))

# methods
print(fs.issubset({1, 2, 3, 9}))
print(fs.union({4}) == {1, 2, 3, 4})
s = set([1, 2])
s.add(3)
s.discard(1)
print(sorted(s))
print(3 in s)

# isinstance
print(isinstance(fs, frozenset))
print(isinstance(a, set))
print(isinstance(fs, set))
print(isinstance(a, frozenset))
