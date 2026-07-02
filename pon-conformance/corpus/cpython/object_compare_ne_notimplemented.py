# Derived from CPython v3.14.0 Lib/test/test_compare.py topics (PSF license).

class Token:
    def __init__(self, name):
        self.name = name


class MissingEquality:
    def __eq__(self, other):
        return NotImplemented


class AlsoMissing:
    def __eq__(self, other):
        return NotImplemented


first = Token("same")
second = Token("same")
alias = first
print(first == second)
print(first != second)
print(first == alias)
print(first != alias)

left = MissingEquality()
right = AlsoMissing()
print(left == right)
print(left != right)
print(left == left)
print(left != left)
