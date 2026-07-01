class Base:
    def __init__(self, value):
        self.value = value

    def label(self):
        return f"base:{self.value}"

class Child(Base):
    def __init__(self, value):
        super().__init__(value + 1)

    def label(self):
        return "child->" + super().label()

obj = Child(4)
print(obj.label())
print(isinstance(obj, Base), isinstance(obj, Child))
