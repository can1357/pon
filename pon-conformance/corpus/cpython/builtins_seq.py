class Rev:
    def __reversed__(self):
        return iter([9, 8])

class DirOnly:
    def __dir__(self):
        return ["z", "a"]

items = [3, 1, 2, 4]
print(dir(DirOnly()))
print(min(items), max(items), min(3, 1, 2), max(3, 1, 2, key=lambda x: -x))
print(min([], default=9), max([], default=8))
try:
    print(min(1, 2, default=0))
except Exception as exc:
    print(str(exc))
print(sum([1, 2, 3]), sum([1, 2], start=10))
print(sorted(items), sorted(items, key=lambda x: x % 2), sorted(items, key=lambda x: x % 2, reverse=True))
print(list(reversed([1, 2, 3])), list(reversed(Rev())))
s = slice(1, 5, 2)
t = slice(3)
print(s.start, s.stop, s.step, s.indices(6))
print(t.start, t.stop, t.step, t.indices(10))
