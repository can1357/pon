# Derived from CPython v3.14.0 Lib/test/test_unpack.py topics (PSF license).

data = ("root", [1, 2, 3, 4], ("leaf", ("x", "y")))
head, (first, *middle, last), (name, (left, right)) = data
print("nested", head, first, middle, last, name, left, right)

only_first, *empty_middle, only_last = [10, 20]
print("empty-star", only_first, empty_middle, only_last)

*a, b, c = range(5)
print("leading-star", a, b, c)

a, b, *c = range(2)
print("trailing-empty", a, b, c)

pairs = [[1, 2], [3, 4], [5, 6]]
collected = []
for left_value, right_value in pairs:
    collected.append(left_value + right_value)
print("for-target", collected)

rows = [("a", 1, 2), ("b", 3, 4, 5)]
for label, *numbers in rows:
    total = 0
    for number in numbers:
        total += number
    print("row", label, numbers, total)

first_row, second_row = pairs[0], pairs[1]
(left_a, right_a), (left_b, right_b) = first_row, second_row
print("nested-pairs", left_a, right_a, left_b, right_b)
