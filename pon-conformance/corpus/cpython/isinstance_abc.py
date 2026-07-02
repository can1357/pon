from abc import ABCMeta


class Stringish(metaclass=ABCMeta):
    pass


class Control(metaclass=ABCMeta):
    pass


Stringish.register(str)

print(isinstance("x", Stringish))
print(isinstance("x", Control))
print(isinstance(3, Stringish))
print(issubclass(str, Stringish))
print(issubclass(str, Control))
print(issubclass(int, Stringish))
print(issubclass(bool, Stringish))

print(isinstance("x", (Stringish,)))
print(isinstance("x", (int, Stringish)))
print(isinstance(3, (int, Stringish)))
print(isinstance(3.5, (int, Stringish)))
print(issubclass(str, (int, Stringish)))
print(issubclass(str, (int, float)))
print(isinstance(True, int))
print(isinstance(True, (int,)))
print(isinstance("x", int | str))


class MyStr(str):
    pass


print(isinstance(MyStr("y"), Stringish))
print(issubclass(MyStr, Stringish))


class Registered:
    pass


class SubRegistered(Registered):
    pass


class Plain:
    pass


Control.register(Registered)
print(isinstance(Registered(), Control))
print(isinstance(SubRegistered(), Control))
print(issubclass(SubRegistered, Control))
print(isinstance(Plain(), Control))
print(issubclass(Plain, Control))


class Concrete(Stringish):
    pass


print(isinstance(Concrete(), Stringish))
print(issubclass(Concrete, Stringish))
print(isinstance(Concrete(), Control))

# repeated checks take the _abc cache path
print(isinstance("y", Stringish))
print(issubclass(str, Stringish))

counts = {"inst": 0, "sub": 0}


class CountingMeta(type):
    def __instancecheck__(cls, instance):
        counts["inst"] += 1
        return super().__instancecheck__(instance)

    def __subclasscheck__(cls, subclass):
        counts["sub"] += 1
        return super().__subclasscheck__(subclass)


class Counted(metaclass=CountingMeta):
    pass


class SubCounted(Counted):
    pass


c = Counted()
s = SubCounted()

# exact type(obj) is cls: the hook is NOT consulted
print(isinstance(c, Counted), counts["inst"])
print(isinstance(s, Counted), counts["inst"])
print(isinstance(42, Counted), counts["inst"])
# tuple entries recurse with the same fast path per entry
print(isinstance(c, (str, Counted)), counts["inst"])
print(isinstance(s, (str, Counted)), counts["inst"])
print(isinstance(42, (str, Counted)), counts["inst"])
print(isinstance(c, (Counted, str)), counts["inst"])
# issubclass has no exact fast path for custom metatypes
print(issubclass(SubCounted, Counted), counts["sub"])
print(issubclass(Counted, Counted), counts["sub"])
print(issubclass(int, Counted), counts["sub"])
print(issubclass(int, (Counted, int)), counts["sub"])
