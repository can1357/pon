s = iter("abc")
print(next(s), next(s), next(s))
try:
    next(s)
except StopIteration:
    print("str exhausted")
print(list(iter("")), list(iter("héllo")), [c for c in "xyz"])
fresh = iter("ab")
print(iter(fresh) is fresh, iter(fresh) is iter("ab"))
print(type(iter("a")) is type(iter("b")), type(iter("a")) is type(iter([])))
chars = []
for ch in "chain":
    chars.append(ch)
print(chars)
print(list(iter(iter(iter("ab")))))
print(set("aabbc") == {"a", "b", "c"}, sorted(set("cab")))

big = 1 << 1000
r = range(big)
it = iter(r)
print(type(it).__name__, iter(it) is it)
print(next(it), next(it), next(it))
print(type(iter(range(0))) is type(iter(range(big))))
down = iter(range(big, big - 7, -3))
print(next(down) - big, next(down) - big, next(down) - big)
try:
    next(down)
except StopIteration:
    print("longrange exhausted")
print(bool(range(big)), bool(range(big, big)))
print([v - big for v in range(big - 2, big + 2)])
print(next(iter(range(-big, 0))) + big)
mixed = iter(range(5, -big, -(1 << 80)))
print(next(mixed), next(mixed) + (1 << 80) - 5)
print(list(range(True)))
try:
    range(big, 0, 0)
except ValueError as exc:
    print("ValueError", exc)
try:
    range(1.5)
except TypeError:
    print("range float TypeError")

print(list(iter([1, 2, 3])), list(iter((4, 5))), list(iter(range(4))))
print(sorted(iter({10, 11, 12})))
d = {"b": 1, "a": 2}
print(sorted(iter(d)), sorted(iter(d.keys())), sorted(iter(d.values())))
print(sorted(iter(d.items())))
print(list(iter(zip([1, 2], "ab"))), list(iter(enumerate("xy"))))
print(list(iter(reversed([1, 2, 3]))), list(reversed("ab")))
zi = iter(zip("ab", "cd"))
print(iter(zi) is zi, next(zi))
ei = iter(enumerate([7]))
print(iter(ei) is ei, next(ei))
print(list(iter(iter([1, 2]))))
li = iter([9])
print(next(li))
try:
    next(li)
except StopIteration:
    print("list exhausted")
ti = iter(())
try:
    next(ti)
except StopIteration:
    print("tuple exhausted")
ri = iter(range(2, 2))
try:
    next(ri)
except StopIteration:
    print("range exhausted")
