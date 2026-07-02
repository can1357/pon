class Recorder:
    def __set_name__(self, owner, name):
        print("set_name", owner.__name__, name)
        self.owner = owner
        self.name = name

class Base:
    def __init_subclass__(cls, label, **kwargs):
        print("init_subclass", cls.__name__, label, len(kwargs))
        cls.label = label

class Child(Base, label="child"):
    field = Recorder()

print(Child.label)
print(Child.field.owner.__name__, Child.field.name)

class GrandChild(Child, label="grand", extra=1):
    other = Recorder()

print(GrandChild.label)
print(GrandChild.other.owner.__name__, GrandChild.other.name)
