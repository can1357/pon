class Meta(type):
    @classmethod
    def __prepare__(mcls, name, bases, **kwargs):
        print("prepare", name, sorted(kwargs.items()))
        return {}

    def __new__(mcls, name, bases, ns, **kwargs):
        print("meta_new", name, sorted(kwargs.items()))
        return super().__new__(mcls, name, bases, ns)

    def __init__(cls, name, bases, ns, **kwargs):
        print("meta_init", name, sorted(kwargs.items()))
        super().__init__(name, bases, ns)


kwds = {"flavor": "sour", "count": 2}


class A(metaclass=Meta, **kwds):
    pass


class Base:
    def __init_subclass__(cls, **kwargs):
        super().__init_subclass__()
        cls.seen = sorted(kwargs.items())


class B(Base, **{"tag": 7}):
    pass


print("init_subclass", B.seen)


class C(Base, a=1, b=2, **{"c": 3}):
    pass


print("merged", C.seen)


class D(Base, **{"x": 1}, y=2, **{"z": 3}):
    pass


print("multi_dstar", D.seen)

bases = (Base,)


class E(*bases):
    pass


print("starred_base", issubclass(E, Base), E.seen)


class F(*bases, mix=True, **{"more": False}):
    pass


print("starred_mix", F.seen)


def f(*args: *(int,)):
    return args


print("star_annotation", f.__annotations__)


def g(a, b):
    return *a, b


print("return_star", g((1, 2), 3))
print("return_star_empty", g((), "solo"))
