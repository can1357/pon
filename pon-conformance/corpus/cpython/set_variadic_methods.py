# Variadic set operations (CPython `union(*others)` family) plus
# intersection_update; meson_cpu chains `set.union(a, b, ...)` with eight
# operands.
s = {1, 2, 3}
print(sorted(s.union({3, 4}, [5], (6,), {7: 'x'})))
print(sorted(s.union()))
print(sorted(s.intersection({2, 3, 4}, [3, 2])))
print(sorted(s.intersection()))
print(sorted(s.difference({1}, [2])))
print(sorted(s.difference()))

f = frozenset('abc')
u = f.union('cd', 'e')
print(type(u).__name__, sorted(u))
print(type(f.intersection('ab', 'b')).__name__, sorted(f.intersection('ab', 'b')))

t = {1, 2, 3, 4}
t.intersection_update({2, 3, 4}, [3, 4, 5])
print(sorted(t))
t = {1, 2, 3}
t.intersection_update()
print(sorted(t))
u = {1, 2, 3, 4}
u.difference_update({1}, [2])
print(sorted(u))
u.update({9}, [10])
print(sorted(u))
print(sorted(s))  # receivers of non-update ops unchanged
