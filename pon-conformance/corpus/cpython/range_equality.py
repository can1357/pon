# Ranges compare as the sequences they denote (CPython range_richcompare):
# equality ignores spelling, ordering is unsupported, hash agrees with eq.

# Equal and unequal ranges.
print(range(2) == range(2))
print(range(2) == range(3))
print(range(2) != range(2))
print(range(2) != range(3))
print(range(0, 5) == range(5))
print(range(1, 5) == range(5))
print(range(0, 10, 3) == range(0, 10, 3))
print(range(0, 10, 3) == range(0, 10, 4))

# Normalized step: trailing overshoot does not change the denoted sequence.
print(range(0, 3, 2) == range(0, 4, 2))
print(range(0, 3, 2) != range(0, 4, 2))
print(list(range(0, 3, 2)) == list(range(0, 4, 2)))

# Single-element ranges ignore step entirely.
print(range(5, 6, 100) == range(5, 6, 1))
print(range(5, 6, 100) == range(5, 7, 100))

# Negative steps normalize the same way.
print(range(10, 0, -2) == range(10, 1, -2))
print(range(10, 0, -2) == range(10, 0, -3))

# Empty ranges are all equal regardless of bounds and step.
print(range(0) == range(2, 2, 3))
print(range(0) == range(5, 1))
print(range(4, 4, -7) == range(0, 0, 9))
print(range(0) != range(2, 2, 3))
print(range(0) == range(1))

# Ranges never equal non-range sequences or scalars.
print(range(3) == [0, 1, 2])
print(range(3) == (0, 1, 2))
print(range(0) == ())
print(range(2) == 2)
print(range(2) != "range(0, 2)")

# Ordering comparisons are unsupported in every direction.
for expression in ("lt", "le", "gt", "ge"):
    try:
        if expression == "lt":
            range(2) < range(3)
        elif expression == "le":
            range(2) <= range(2)
        elif expression == "gt":
            range(3) > range(2)
        else:
            range(3) >= range(3)
        print("no error", expression)
    except TypeError as error:
        print(type(error).__name__, error)

# Hash agrees with equality, including normalized spellings.
print(hash(range(2)) == hash(range(2)))
print(hash(range(0, 3, 2)) == hash(range(0, 4, 2)))
print(hash(range(5, 6, 100)) == hash(range(5, 6, 1)))
print(hash(range(0)) == hash(range(2, 2, 3)))
print(hash(range(2)) == hash(range(3)))

# Equal ranges collapse onto one dict key slot.
d = {range(3): "plain", range(0, 10, 3): "stepped", range(0): "empty"}
print(d[range(0, 3, 1)])
print(d[range(0, 12, 3)])
print(d[range(7, 7, -2)])
print(len(d))
d[range(0, 4, 2)] = "two-step"
print(d[range(0, 3, 2)])
print(len(d))

# Ranges with bounds beyond machine words follow the same sequence semantics.
big = 10**30
print(range(big) == range(big))
print(range(big) == range(big + 1))
print(range(0, big, 7) == range(0, big + 3, 7))
print(range(big, big) == range(0))
print(range(big, big + 1, big) == range(big, big + 1, 1))
print(hash(range(0, big, 7)) == hash(range(0, big + 3, 7)))
print(hash(range(big, big)) == hash(range(0)))
print({range(0, big, 7): "long"}[range(0, big + 3, 7)])
try:
    range(big) < range(big + 1)
except TypeError as error:
    print(type(error).__name__, error)
