# Derived from CPython v3.14.0 Lib/test/test_binop.py topics (PSF license).

class Low:
    def __lt__(self, other):
        return NotImplemented

    def __le__(self, other):
        return NotImplemented


class High:
    def __gt__(self, other):
        return NotImplemented

    def __ge__(self, other):
        return NotImplemented


def show_order(label, left, right):
    try:
        print(label, left < right)
    except TypeError:
        print(label, "TypeError")


def show_le(label, left, right):
    try:
        print(label, left <= right)
    except TypeError:
        print(label, "TypeError")


show_order("lt", Low(), High())
show_le("le", Low(), High())
print(1 < 2 <= 2)
print(3 > 2 >= 2)
