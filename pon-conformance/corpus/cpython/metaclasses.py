Type = type(type(0))

class Meta(Type):
    pass

class Base(metaclass=Meta):
    marker = "base"

class Child(Base):
    pass

print(type(Base).__name__)
print(type(Child).__name__)
print(Child.marker)

Made = type("Made", (), {"value": 5})
print(Made.__name__)
print(Made().value)

class Replacement:
    pass

class Proxy:
    def __mro_entries__(self, bases):
        print("mro_entries", len(bases))
        return (Replacement,)

class UsesProxy(Proxy()):
    pass

print(isinstance(UsesProxy(), Replacement))

class OtherMeta(Type):
    pass

class OtherBase(metaclass=OtherMeta):
    pass

try:
    class Bad(Base, OtherBase):
        pass
except TypeError:
    print("metaclass conflict")
