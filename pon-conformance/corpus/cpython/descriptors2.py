print("descriptors")


class NonData:
    def __get__(self, obj, cls):
        if obj is None:
            return "nondata class"
        return "nondata descriptor"


class Data:
    def __get__(self, obj, cls):
        if obj is None:
            return "data class"
        return "data descriptor:" + obj.storage

    def __set__(self, obj, value):
        events.append("set:" + value)
        obj.storage = value

    def __delete__(self, obj):
        events.append("delete:" + obj.storage)
        obj.storage = "deleted"


events = []


class Holder:
    nondata = NonData()
    data = Data()

    def __init__(self):
        self.storage = "initial"


holder = Holder()
holder.__dict__ = {"storage": "initial", "nondata": "instance nondata", "data": "instance data"}
print("nondata precedence", holder.nondata)
print("data precedence", holder.data)
holder.data = "assigned"
print("data after set", holder.data, holder.__dict__["data"], events)
del holder.data
print("data after delete", holder.data, events)
print("class access", Holder.nondata, Holder.data)


class WithProperty:
    def __init__(self):
        self._value = "ready"

    @property
    def value(self):
        return self._value

    @value.setter
    def value(self, new_value):
        prop_events.append("set:" + new_value)
        self._value = new_value

    @value.deleter
    def value(self):
        prop_events.append("delete:" + self._value)
        del self._value


prop_events = []
prop = WithProperty()
print("property get", prop.value)
prop.value = "changed"
print("property set", prop.value, prop_events)
del prop.value
print("property deleted", hasattr(prop, "_value"), prop_events)
try:
    prop.value
except AttributeError:
    print("property missing")


holder.keep = "old"
holder.__dict__ = {"storage": "fresh", "nondata": "replacement nondata"}
print("dict replaced", holder.storage, hasattr(holder, "keep"), holder.nondata, holder.data)
try:
    holder.__dict__ = [("storage", "bad")]
except TypeError:
    print("dict assignment TypeError")


class Alpha:
    def kind(self):
        return "alpha"


class Beta:
    def kind(self):
        return "beta"


obj = Alpha()
obj.name = "kept"
print("class before", obj.kind(), obj.__class__.__name__)
obj.__class__ = Beta
print("class after", obj.kind(), obj.__class__.__name__, obj.name)
try:
    obj.__class__ = int
except TypeError:
    print("class assignment TypeError")
