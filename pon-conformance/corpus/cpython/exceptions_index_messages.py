def check(label, fn):
    try:
        fn()
        print(label, "no-error")
    except (IndexError, KeyError) as e:
        print(label, type(e).__name__, str(e))


check("list-get", lambda: [1, 2][9])


def list_set_oob():
    x = [1, 2]
    x[9] = 0


check("list-set", list_set_oob)


def list_del_oob():
    x = [1, 2]
    del x[9]


check("list-del", list_del_oob)
check("list-pop-empty", lambda: [].pop())
check("list-pop-oob", lambda: [1].pop(9))
check("tuple-get", lambda: (1, 2)[9])
check("bytes-get", lambda: b"ab"[9])
check("bytearray-get", lambda: bytearray(b"ab")[9])


def bytearray_set_oob():
    ba2 = bytearray(b"ab")
    ba2[9] = 0


check("bytearray-set", bytearray_set_oob)
check("str-get", lambda: "ab"[9])
check("range-get", lambda: range(3)[9])
check("dict-missing", lambda: {}["missing"])


for exc in [
    KeyError("k"),
    KeyError(5),
    KeyError(),
    KeyError(1, 2),
    ValueError("boom"),
    ValueError(42),
    IndexError("x"),
    RuntimeError("a", "b"),
    TypeError("t"),
    StopIteration(42),
    StopIteration(),
    Exception(),
    Exception("solo"),
    Exception(3.5),
]:
    print(type(exc).__name__, repr(exc), str(exc))
