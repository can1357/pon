# Derived from CPython v3.14.0 Lib/test/test_binop.py topics (PSF license).

log = []


class Left:
    def __add__(self, other):
        log.append("Left.__add__")
        return NotImplemented


class Right:
    def __radd__(self, other):
        log.append("Right.__radd__")
        return "right-added"


print(Left() + Right())
print(log)

log = []


class Base:
    def __add__(self, other):
        log.append("Base.__add__")
        return "base-add"


class Sub(Base):
    def __radd__(self, other):
        log.append("Sub.__radd__")
        return "sub-radd"


print(Base() + Sub())
print(log)
log = []
print(Sub() + Base())
print(log)
