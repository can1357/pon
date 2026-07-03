# dir() special-method dispatch: `dir(obj)` calls `type(obj).__dir__(obj)`
# (never instance getattr), so an instance-facing `__dir__` on a class must
# NOT fire for the class object itself (unittest.mock imports depend on
# `dir(NonCallableMock)`), while a METACLASS `__dir__` does fire for its
# classes.  `type.__dir__` merges the class's own MRO dicts and deliberately
# excludes metaclass attributes.

class InstDir:
    def __dir__(self):
        return ["zeta", "alpha", "alpha"]

    def helper(self):
        pass


names = dir(InstDir)
print("helper" in names, "__dir__" in names, "zeta" in names)

# Instance path: __dir__ result is sorted and deduplicated by dir().
print(dir(InstDir()))


class Base:
    def base_meth(self):
        pass


class Child(Base):
    def child_meth(self):
        pass


child_names = dir(Child)
print("child_meth" in child_names, "base_meth" in child_names)

# Inherited instance __dir__ fires for subclass instances too.
class SubInstDir(InstDir):
    pass


print(dir(SubInstDir()))


class Meta(type):
    def __dir__(cls):
        return ["meta_dir_fired", "beta"]


class WithMeta(metaclass=Meta):
    pass


print(dir(WithMeta))

# Metaclass attributes stay out of plain class dir() (CPython type.__dir__).
print("mro" in dir(int), "mro" in dir(Child))

# Immediate (tagged) receivers and instance dict entries.
print(type(dir(1)) is list)


class WithDict:
    def method_a(self):
        pass


w = WithDict()
w.inst_attr = 1
wn = dir(w)
print("inst_attr" in wn, "method_a" in wn)
