# Derived from CPython v3.14.0 Lib/test/test_funcattrs.py topics (PSF license).

class Sample:
    def __init__(self, base):
        self.base = base

    def add(self, value):
        return self.base + value

    def combine(self, left, right):
        return self.base + left + right


first = Sample(10)
second = Sample(20)
first_add = first.add
second_add = second.add
first_combine = first.combine

print(first_add(5))
print(second_add(5))
print(first.add(6))
print(Sample.add(first, 7))
print(first_combine(1, 2))

methods = [first_add, second_add, first_combine]
print(methods[0](1))
print(methods[1](1))
print(methods[2](1, 1))
print(first_add is first.add)
