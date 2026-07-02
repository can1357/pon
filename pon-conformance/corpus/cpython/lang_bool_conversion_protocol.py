# Derived from CPython v3.14.0 Lib/test/test_bool.py topics (PSF license).

class ReturnsSelf:
    def __bool__(self):
        return self


class ReturnsString:
    def __bool__(self):
        return "yes"


class ReturnsInt:
    def __bool__(self):
        return 1


class NegativeLen:
    def __len__(self):
        return -1


class RaisesFromBool:
    def __bool__(self):
        raise TypeError("symbolic")


def check(label, value):
    try:
        result = bool(value)
    except Exception as exc:
        print(label, type(exc).__name__)
    else:
        print(label, result)


check("self", ReturnsSelf())
check("string", ReturnsString())
check("int", ReturnsInt())
check("negative-len", NegativeLen())
try:
    if RaisesFromBool():
        print("branch true")
    else:
        print("branch false")
except TypeError as exc:
    print("if", type(exc).__name__, exc.args[0])
