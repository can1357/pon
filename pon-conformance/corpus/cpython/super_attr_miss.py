# Derived from CPython v3.14.0 Lib/test/test_super.py topics (PSF license):
# super() attribute misses raise a catchable AttributeError with CPython
# wording, hits resolve through every MRO position, and the cooperative
# __init_subclass__ / __new__-over-builtin chains terminate cleanly.

class Base:
    shared = "base-shared"

    def hop(self):
        return "Base.hop"


class MidA(Base):
    def hop(self):
        return "MidA->" + super().hop()

    def only_mid_a(self):
        return "MidA.only"


class MidB(Base):
    shared = "midb-shared"

    def hop(self):
        return "MidB->" + super().hop()


class Leaf(MidA, MidB):
    def hop(self):
        return "Leaf->" + super().hop()

    def from_mid_a(self):
        return super(MidA, self).hop()

    def from_mid_b(self):
        return super(MidB, self).hop()

    def shared_from_each(self):
        return (super().shared, super(MidA, self).shared, super(MidB, self).shared)


leaf = Leaf()
# Attr found via each MRO position: zero-arg and 2-arg forms.
print(leaf.hop())
print(leaf.from_mid_a())
print(leaf.from_mid_b())
print(super(Leaf, leaf).only_mid_a())
print(leaf.shared_from_each())

# Missing attribute: typed, catchable, exact CPython wording.
try:
    super(Leaf, leaf).nowhere
except AttributeError as exc:
    print("zero-pos miss:", exc)

class Prober(Base):
    def probe(self):
        try:
            super().gone_missing
        except AttributeError as exc:
            return f"caught: {exc}"
        return "unreached"

print(Prober().probe())

try:
    super(Base, leaf).hop
except AttributeError as exc:
    print("past-terminus miss:", exc)


# Cooperative __init_subclass__ chain ends at object's no-op.
class HookRoot:
    seen = []

    def __init_subclass__(cls, **kwargs):
        super().__init_subclass__(**kwargs)
        HookRoot.seen.append(cls.__name__)


class HookMid(HookRoot):
    def __init_subclass__(cls, **kwargs):
        super().__init_subclass__(**kwargs)
        HookRoot.seen.append("via-mid:" + cls.__name__)


class HookLeaf(HookMid):
    pass


print(HookRoot.seen)


# super().__new__ over a builtin-base MRO position (importlib bootstrap's
# KeyedRef shape): the subclass constructs through the base's __new__ and
# instance state lives on the subclass.
import _weakref


class Anchor:
    pass


class KeyedRef(_weakref.ref):
    __slots__ = "key",

    def __new__(type, ob, key):
        self = super().__new__(type, ob, type.remove)
        self.key = key
        return self

    def __init__(self, ob, key):
        super().__init__(ob, self.remove)

    @staticmethod
    def remove(wr):
        return wr.key


anchor = Anchor()
ref = KeyedRef(anchor, "lock-name")
print(type(ref).__name__, ref.key, ref() is anchor)
print(isinstance(ref, _weakref.ref))

try:
    super(KeyedRef, ref).missing_on_ref
except AttributeError as exc:
    print("builtin-base miss:", exc)
