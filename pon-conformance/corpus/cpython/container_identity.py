# Container identity across literal and constructor representations.

# tuple(iterable) results must be structurally equal to tuple literals,
# including as dict keys (hash + eq agreement).
d = {(1, 2): "x"}
print(d[tuple([1, 2])])
d2 = {tuple([3, 4]): "y"}
print(d2[(3, 4)])
print(tuple([1, 2]) == (1, 2))
print((1, 2) == tuple([1, 2]))
print(hash(tuple([1, 2])) == hash((1, 2)))

# list constructor round-trips.
print(list("ab"))
print(list("ab") == ["a", "b"])
print(list((1, 2, 3)))
print(list(range(3)))
print(tuple("ab"))
print(tuple())
print(list())

# type() identity for constructor outputs.
print(type(tuple([1, 2])) is tuple)
print(type(tuple([1, 2])) is type((1, 2)))
print(type(list("ab")) is list)
print(type(list()) is list)
print(type(tuple()) is tuple)
print(type(sorted([2, 1])) is list)
print(type(list("ab") + ["c"]) is list)
print(type(tuple([1]) + (2,)) is tuple)

# sorted() returns a real list.
print(sorted([3, 1, 2]))
print(sorted((3, 1, 2)))
print(sorted("cba"))
s = sorted([2, 1])
s.append(9)
print(s)
print(sorted([3, 1, 2], reverse=True))

# reversed/enumerate/zip outputs are usable as expected.
print(list(reversed([1, 2, 3])))
print(list(enumerate("ab")))
print(list(zip([1, 2], "ab")))
pairs = list(zip([1, 2], [3, 4]))
print(pairs[0] == (1, 3))
print(type(pairs[0]) is tuple)
d3 = {}
for pair in zip([1], ["one"]):
    d3[pair] = True
print(d3[(1, "one")])
d4 = {}
for pair in enumerate(["a"]):
    d4[pair] = "seen"
print(d4[(0, "a")])

# str.split returns a real, mutable list.
parts = "a,b".split(",")
print(parts)
print(type(parts) is list)
parts.append("c")
print(parts)

# divmod returns a real tuple usable as a dict key.
print(divmod(7, 2) == (3, 1))
print({divmod(7, 2): "ok"}[(3, 1)])

# Nested constructed tuples keep structural equality.
print({(1, (2, 3)): "n"}[tuple([1, tuple([2, 3])])])

# Constructed containers concatenate with literals.
print(tuple([1, 2]) + (3,))
print((0,) + tuple([1]))
print(list("ab") + ["c"])
print(["z"] + list("ab"))

# Constructed tuples index, slice, unpack, and iterate.
t = tuple([10, 20, 30])
print(t[0], t[-1])
print(t[1:])
a, b, c = t
print(a, b, c)
print([v for v in tuple("xy")])
print(len(tuple([1, 2, 3])), len(list("abcd")))

# Constructed containers compare and hash inside sets.
print(tuple([1, 2]) in {(1, 2)})
print((5, 6) in {tuple([5, 6])})

# isinstance agreement for constructor outputs.
print(isinstance(tuple([1]), tuple))
print(isinstance(list("a"), list))
print(isinstance(sorted([1]), list))
