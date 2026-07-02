# Derived from CPython v3.14.0 Lib/test/test_super.py topics (PSF license).

class Root:
    def mark(self):
        return "R"


class Left(Root):
    def mark(self):
        return "L" + super().mark()


class Right(Root):
    def mark(self):
        return "M" + super().mark()


class Child(Left, Right):
    def via_child(self):
        return super().mark()

    def via_left(self):
        return super(Left, self).mark()

    def via_right(self):
        return super(Right, self).mark()

    def all_marks(self):
        return (self.mark(), self.via_child(), self.via_left(), self.via_right())


class GrandChild(Child):
    def mark(self):
        return "G" + super().mark()


child = Child()
grand = GrandChild()
print(child.all_marks())
print(grand.mark())
print(super(Child, grand).mark())
print(super(Left, grand).mark())
