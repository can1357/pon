# Derived from CPython v3.14.0 Lib/test/test_unpack.py topics (PSF license).

def too_many_consumes_one_extra():
    it = iter(range(8))
    a = "old-a"
    b = "old-b"
    c = "old-c"
    try:
        a, b, c = it
    except ValueError as exc:
        print("too-many", type(exc).__name__, next(it), a, b, c)


def not_enough_keeps_old_targets():
    a = "old-a"
    b = "old-b"
    c = "old-c"
    try:
        a, b, c = iter([1, 2])
    except ValueError as exc:
        print("not-enough", type(exc).__name__, a, b, c)


def non_iterable_error():
    a = "left"
    b = "right"
    try:
        a, b = 7
    except TypeError as exc:
        print("non-iterable", type(exc).__name__, a, b)


def nested_error():
    pair = "original"
    try:
        (pair, (inner_left, inner_right)) = (1, (2, 3, 4))
    except ValueError as exc:
        print("nested-error", type(exc).__name__, pair)


too_many_consumes_one_extra()
not_enough_keeps_old_targets()
non_iterable_error()
nested_error()
