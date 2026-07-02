# Derived from CPython v3.14.0 Lib/test/test_property.py topics (PSF license).

class Base:
    def __init__(self, value):
        self._value = value

    @property
    def value(self):
        return self._value


class Offset(Base):
    @property
    def value(self):
        return self._value + 100


class Doubled(Base):
    @property
    def value(self):
        return self._value * 2


class Pair:
    def __init__(self, left, right):
        self.left = left
        self.right = right

    @property
    def total(self):
        return self.left + self.right


print(Base(5).value)
print(Offset(5).value)
print(Doubled(5).value)
pair = Pair(3, 4)
print(pair.total)
pair.left = 10
print(pair.total)
