# Derived from CPython v3.14.0 Lib/test/test_iter.py topics (PSF license).

class Squares:
    def __init__(self, limit):
        self.limit = limit

    def __getitem__(self, index):
        if index < 0:
            raise IndexError
        if index < self.limit:
            return index * index
        raise IndexError


def collect(seq):
    result = []
    for value in seq:
        result.append(value)
    return result


print("for", collect(Squares(4)))
a, b, c = Squares(3)
print("unpack", a, b, c)
try:
    a, b = Squares(3)
except ValueError as exc:
    print("too-many", type(exc).__name__)
try:
    a, b, c, d = Squares(3)
except ValueError as exc:
    print("not-enough", type(exc).__name__)
try:
    a, b = Squares(0)
except ValueError as exc:
    print("empty", type(exc).__name__)
