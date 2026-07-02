# Derived from CPython v3.14.0 Lib/test/test_yield_from.py topics (PSF license).


def inner(value):
    yield "inner"
    return value


def outer():
    yield "outer"
    first = yield from inner(None)
    yield "ret " + str(first)
    second = yield from inner(7)
    yield "ret " + str(second)
    third = yield from inner((2, 3))
    yield "ret " + str(third)


items = []
for value in outer():
    items.append(value)
print(items)

again = outer()
print(next(again))
print(next(again))
try:
    while True:
        next(again)
except StopIteration as exc:
    print(type(exc).__name__)
