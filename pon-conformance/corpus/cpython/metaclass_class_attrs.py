class Meta(type):
    counter = 0

    def __new__(mcls, name, bases, namespace, **kwargs):
        cls = super().__new__(mcls, name, bases, namespace)
        cls.tag = "made-" + name
        cls.order = Meta.counter
        Meta.counter += 1
        return cls

    def describe(cls):
        return cls.__name__ + ":" + cls.tag


class Base(metaclass=Meta):
    marker = "base"


class Child(Base):
    pass


print(Base.tag)
print(Child.tag)
print(Base.order, Child.order)
print(Meta.counter)

print(Base.describe())
print(Child.describe())

b = Base()
print(b.marker, type(b).tag)

Base.extra = [1, 2]
print(Base.extra, Child.extra)
Child.extra = [3]
print(Base.extra, Child.extra)

Made = Meta("Made", (Base,), {"value": 5})
print(type(Made).__name__, Made.tag, Made.value, Made.order)
setattr(Made, "value", 6)
print(Made.value)
print("tag" in Made.__dict__, "value" in Made.__dict__)
print(sorted(k for k in Base.__dict__ if not k.startswith("__")))

class InitMeta(type):
    def __init__(cls, name, bases, namespace, **kwargs):
        cls.init_seen = name


class WithInit(metaclass=InitMeta):
    pass


print(WithInit.init_seen)

class NsMeta(type):
    def __new__(mcls, name, bases, namespace):
        namespace["injected"] = name.upper()
        return super().__new__(mcls, name, bases, namespace)


class Injected(metaclass=NsMeta):
    pass


print(Injected.injected)
print("injected" in Injected.__dict__)

class MetaOuter(Meta):
    def __new__(mcls, name, bases, namespace, **kwargs):
        cls = super().__new__(mcls, name, bases, namespace, **kwargs)
        cls.outer = "outer-" + name
        return cls


class Deep(metaclass=MetaOuter):
    pass


print(Deep.tag, Deep.outer)
print(type(Deep).__name__)
print([t.__name__ for t in type(Deep).__mro__])


class MetaHollow(Meta):
    pass


class Shallow(metaclass=MetaHollow):
    pass


print(Shallow.tag, type(Shallow).__name__)

from abc import ABCMeta, abstractmethod


class AbstractThing(metaclass=ABCMeta):
    @abstractmethod
    def act(self):
        ...


try:
    AbstractThing()
    print("no-error")
except TypeError:
    print("abstract-typeerror")


class Concrete(AbstractThing):
    def act(self):
        return "acted"


c = Concrete()
print(c.act())
print(issubclass(Concrete, AbstractThing), isinstance(c, AbstractThing))


class Foreign:
    pass


AbstractThing.register(Foreign)
print(issubclass(Foreign, AbstractThing), isinstance(Foreign(), AbstractThing))


class Unrelated:
    pass


print(issubclass(Unrelated, AbstractThing), isinstance(Unrelated(), AbstractThing))


class DepthAbc(ABCMeta):
    def __new__(mcls, name, bases, namespace, **kwargs):
        cls = super().__new__(mcls, name, bases, namespace, **kwargs)
        cls.stamped = True
        return cls


class StampedABC(metaclass=DepthAbc):
    @abstractmethod
    def go(self):
        ...


print(StampedABC.stamped, type(StampedABC).__name__)
try:
    StampedABC()
    print("no-error")
except TypeError:
    print("stamped-typeerror")
