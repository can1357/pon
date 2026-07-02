# Lexicographic tuple/list ordering: <, <=, >, >= across equal-prefix,
# mixed-length, and nested shapes; elements dispatch through rich compare
# (user-class elements order via their dunders); subclass instances
# (namedtuple!) resolve the ordering dunders through the tuple/list MRO;
# sort/heapq round-trips ride the same comparators; cross-kind ordering
# (list vs tuple) is the CPython TypeError.

print((1, 2) < (1, 3), (1, 3) < (1, 2), (1, 2) < (1, 2))
print([1, 2] < [1, 3], [1, 3] < [1, 2], [1, 2] < [1, 2])
print((1, 2) <= (1, 2), (1, 2) >= (1, 2), (1, 2) > (1, 2))
print([1, 2] <= [1, 2], [1, 2] >= [1, 2], [1, 2] > [1, 2])

# Mixed length: an exhausted prefix orders by length.
print((1, 2) < (1, 2, 0), (1, 2, 0) <= (1, 2), (1, 2, 0) > (1, 2))
print([1, 2] < [1, 2, 0], [1, 2, 0] <= [1, 2], [1, 2, 0] > [1, 2])
print(() < (0,), [] < [0], () <= (), [] >= [])

# Equal prefix decides at the first differing slot.
print((1, 1, 9) < (1, 2, 0), [1, 1, 9] < [1, 2, 0])
print(("a", "b") < ("a", "c"), ["a", "c"] > ["a", "b"])

# Nested sequences compare element-wise through rich compare.
print(((1, 2), (3,)) < ((1, 2), (3, 1)))
print([(1, "a")] < [(1, "b")], [[1, 2], [3]] < [[1, 2], [3, 0]])

# User-class elements dispatch their own ordering dunders.
class Cell:
    def __init__(self, value):
        self.value = value
    def __eq__(self, other):
        return self.value == other.value
    def __lt__(self, other):
        print("Cell.__lt__", self.value, other.value)
        return self.value < other.value

print((Cell(1), 9) < (Cell(2), 0))
print([Cell(3)] < [Cell(3)])

# Subclass instances resolve ordering through the tuple/list MRO.
class T(tuple):
    pass

class L(list):
    pass

print(T((1, 2)) < T((1, 3)), T((1, 3)) <= T((1, 2)), T((1, 2)) >= T((1, 2)))
print(T((1, 2)) < (1, 3), (1, 2) < T((1, 3)), (1, 4) > T((1, 3)))
print(L([1, 2]) < L([1, 3]), L([1, 2]) < [1, 3], [1, 4] >= L([1, 4]))
print(sorted([T((2, "b")), T((1, "a")), T((1,))]))

from collections import namedtuple
Pt = namedtuple("Pt", ["x", "y"])
print(Pt(1, 2) < Pt(1, 3), Pt(1, 3) <= Pt(1, 2), Pt(2, 0) > Pt(1, 9))
print(sorted([Pt(2, "b"), Pt(1, "a")]))

# Sort round-trips: bare sort(), keyword form, and sorted().
pairs = [(3, "c"), (1, "a"), (2, "b")]
print(sorted(pairs))
copy = list(pairs)
copy.sort()
print(copy)
copy.sort(reverse=True)
print(copy)
print(sorted([[2, "b"], [1, "c"], [1, "a"]]))
print(min(pairs), max(pairs))

# heapq round-trips ride the same tuple comparators.
import heapq
print(heapq.nsmallest(2, pairs))
print(heapq.nlargest(2, pairs))
heap = list(pairs)
heapq.heapify(heap)
print([heapq.heappop(heap) for _ in range(len(pairs))])

# Cross-kind ordering is a TypeError; equality stays False.
try:
    [1, 2] < (1, 3)
except TypeError as exc:
    print("TypeError", exc)
try:
    (1, 2) >= [1, 2]
except TypeError as exc:
    print("TypeError", exc)
print([1, 2] == (1, 2), (1, 2) != [1, 2])
