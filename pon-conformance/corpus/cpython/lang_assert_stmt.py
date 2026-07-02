print("assert statement")


def passing_assert(value):
    assert value
    return "passed"


def failing_assert():
    try:
        assert False, "boom"
    except AssertionError as exc:
        return type(exc).__name__ + ":" + str(exc)


print("assert pass", passing_assert(True))
print("assert fail", failing_assert())
