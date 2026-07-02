# Derived from CPython v3.14.0 Lib/test/test_descr.py topics (PSF license).

class ManualDescriptor:
    def __init__(self, label):
        self.label = label
        self.events = []

    def __get__(self, obj, cls):
        if obj is None:
            return self.label + " from class"
        return self.label + ":" + obj.value

    def __set__(self, obj, value):
        self.events.append("set:" + value)
        obj.value = value


field_descriptor = ManualDescriptor("field")


class Holder:
    field = field_descriptor

    def __init__(self, value):
        self.value = value


item = Holder("first")
print(field_descriptor.__get__(item, Holder))
field_descriptor.__set__(item, "second")
print(field_descriptor.__get__(item, Holder))
print(field_descriptor.__get__(None, Holder))
print(field_descriptor.events)

getter = field_descriptor.__get__
setter = field_descriptor.__set__
setter(item, "third")
print(getter(item, Holder))
