PREPARED = []


class Recorder(dict):
    def __init__(self, cls_name):
        super().__init__()
        self.order = []
        self.cls_name = cls_name

    def __setitem__(self, key, value):
        self.order.append(key)
        if isinstance(value, str) and not key.startswith('__'):
            value = value.upper()
        super().__setitem__(key, value)


class Meta(type):
    @classmethod
    def __prepare__(metacls, name, bases, **kwds):
        print('prepare:', name, [b.__name__ for b in bases], sorted(kwds))
        mapping = Recorder(name)
        PREPARED.append(mapping)
        mapping['_seeded'] = 'seed'
        return mapping

    def __new__(metacls, name, bases, namespace, **kwds):
        cls = super().__new__(metacls, name, bases, namespace)
        cls._order = [k for k in namespace.order if not k.startswith('__')]
        cls._ns_type = type(namespace).__name__
        cls._ns_is_prepared = namespace is PREPARED[-1]
        cls._ns_cls_name = namespace.cls_name
        return cls


class Widget(metaclass=Meta):
    color = 'red'
    size = 42

    def describe(self):
        return (self.color, self.size)


print(Widget._order)
print(Widget._ns_type)
print(Widget._ns_is_prepared)
print(Widget._ns_cls_name)
print(Widget._seeded)
print(Widget.color)
print(Widget.size)
print(Widget().describe())


# Inherited metaclass: __prepare__ fires again for subclasses, with the
# base in the bases tuple.
class Gadget(Widget):
    flavor = 'mint'


print(Gadget._order)
print(Gadget._ns_cls_name)
print(Gadget.flavor)
print(Gadget.color)


# Class keywords reach __prepare__ (metaclass keyword itself is stripped).
class KwMeta(type):
    @classmethod
    def __prepare__(metacls, name, bases, **kwds):
        mapping = dict()
        mapping['_kw_keys'] = tuple(sorted(kwds))
        return mapping

    def __new__(metacls, name, bases, namespace, **kwds):
        return super().__new__(metacls, name, bases, namespace)


class KwClass(metaclass=KwMeta, flag=1, mode='fast'):
    kind = 'kw'


print(KwClass._kw_keys)
print(KwClass.kind)


# Class-body name loads consult the prepared mapping first, then fall back
# to module globals (CPython class-scope LOAD_NAME order).
FALLBACK = 'global'


class Loads(metaclass=Meta):
    local_name = 'inner'
    echoed = local_name
    fell_back = FALLBACK


print(Loads.echoed)
print(Loads.fell_back)


# Mappings may reject writes: the class body surfaces the mapping's error.
class Strict(dict):
    def __setitem__(self, key, value):
        if key == 'forbidden':
            raise ValueError('forbidden name')
        super().__setitem__(key, value)


class StrictMeta(type):
    @classmethod
    def __prepare__(metacls, name, bases, **kwds):
        return Strict()


try:
    class Rejected(metaclass=StrictMeta):
        forbidden = 1
except ValueError as exc:
    print('rejected:', exc)


# The builtin default: type.__prepare__ ignores its arguments and returns a
# fresh empty dict.
print(type.__prepare__())
print(type.__prepare__('x', ()))
print(sorted(type.__prepare__(1, 2, three=3).items()))
d1 = type.__prepare__()
d2 = type.__prepare__()
print(d1 is d2)
