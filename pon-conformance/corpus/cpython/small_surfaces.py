import time
import codecs
from _codecs import _unregister_error
from weakref import WeakSet
import _weakrefset


def new_func(cls):
    return object.__new__(cls)


def plain(x):
    return x


class Carrier:
    __new__ = new_func

    def method(self):
        return "m"

    static_m = staticmethod(plain)
    class_m = classmethod(plain)


# --- __func__ surfaces -----------------------------------------------------
# Class-body `__new__` is implicitly wrapped in a staticmethod carrier.
print("new kind", type(Carrier.__dict__["__new__"]).__name__)
print("new func identity", Carrier.__dict__["__new__"].__func__ is new_func)
print("static func identity", Carrier.__dict__["static_m"].__func__ is plain)
print("class func identity", Carrier.__dict__["class_m"].__func__ is plain)
instance = Carrier()
print("instance kind", type(instance).__name__)
print("method func identity", instance.method.__func__ is Carrier.__dict__["method"])
print("method self identity", instance.method.__self__ is instance)


class SuperNew:
    def __new__(cls):
        return super().__new__(cls)


print("super new kind", type(SuperNew()).__name__)

# Plain functions do NOT grow `__func__`; descriptor probes stay CPython-shaped.
print("plain has func", hasattr(plain, "__func__"))
print("plain has get", hasattr(plain, "__get__"))
print("property has get", hasattr(property(plain), "__get__"))
print("property has func", hasattr(property(plain), "__func__"))

# --- time monotonic family --------------------------------------------------
t1 = time.monotonic()
t2 = time.monotonic()
print("monotonic ordered", t2 >= t1)
print("monotonic type", type(t1).__name__)
n1 = time.monotonic_ns()
n2 = time.monotonic_ns()
print("monotonic_ns ordered", n2 >= n1)
print("monotonic_ns type", type(n1).__name__)
p1 = time.perf_counter()
p2 = time.perf_counter()
print("perf_counter ordered", p2 >= p1)
print("perf_counter_ns type", type(time.perf_counter_ns()).__name__)

# --- codecs error-handler round-trip ----------------------------------------
def handler(exc):
    return ("", exc.end)


codecs.register_error("small-surfaces-handler", handler)
print("registered lookup", codecs.lookup_error("small-surfaces-handler") is handler)
print("unregister known", _unregister_error("small-surfaces-handler"))
try:
    codecs.lookup_error("small-surfaces-handler")
    print("lookup after unregister", "still present")
except LookupError:
    print("lookup after unregister", "LookupError")
print("unregister unknown", _unregister_error("small-surfaces-missing"))
try:
    _unregister_error("strict")
    print("unregister builtin", "allowed")
except ValueError as exc:
    print("unregister builtin", type(exc).__name__, exc)

# --- weakref.WeakSet re-export ----------------------------------------------
print("weakset identity", WeakSet is _weakrefset.WeakSet)
print("weakset name", WeakSet.__name__)


class Item:
    pass


keep = [Item(), Item()]
ws = WeakSet()
ws.add(keep[0])
ws.add(keep[1])
ws.add(keep[0])
print("weakset len", len(ws))
print("weakset contains", keep[0] in ws, Item() in ws)
ws.discard(keep[1])
print("weakset after discard", len(ws), keep[1] in ws)
