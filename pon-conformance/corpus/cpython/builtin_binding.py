import math
import time

# CPython builtins (builtin_function_or_method) are NOT descriptors: stored
# as a class attribute and read off an instance or the class, they come back
# BARE — no bound self is prepended.  User functions ARE descriptors and
# bind.  The runner pins TZ=UTC, so localtime(0) is deterministic.


class Holder:
    conv = time.localtime
    log = math.log
    size = len

    static_conv = staticmethod(time.localtime)

    def method(self, tag):
        return ("bound", tag, type(self).__name__)

    twice = lambda self: 2  # noqa: E731 — user function via lambda, binds too.


h = Holder()

# Native module functions: bare on instance AND class access.
print(h.conv is time.localtime)
print(Holder.conv is time.localtime)
print(h.conv(0)[:6])
print(Holder.conv(0)[:6])
print(h.log is math.log)
print(h.log(math.e))

# Builtins from the builtins namespace behave the same.
print(h.size is len)
print(h.size([1, 2, 3]))

# Repeated reads keep returning the same bare object.
print(h.conv is h.conv)

# Inheritance: the MRO walk still returns the bare function.
class Child(Holder):
    pass


c = Child()
print(c.conv is time.localtime)
print(c.conv(0)[0])

# User-function contrast: instance access binds a fresh method each read.
print(h.method("x"))
print(h.method is h.method)
print(Holder.__dict__["method"] is h.method)
print(h.twice())

# staticmethod contrast: unwraps to the same bare function on any access.
print(h.static_conv is time.localtime)
print(Holder.static_conv is time.localtime)
print(h.static_conv(0)[0])

# Descriptor-protocol surface (enum's _is_descriptor probe): native
# functions expose no __get__/__set__/__delete__; user functions expose
# __get__ only.
for target in (time.localtime, math.log, len, Holder.__dict__["method"]):
    print(hasattr(target, "__get__"), hasattr(target, "__set__"), hasattr(target, "__delete__"))

# getattr() spelling routes through the same lookup.
print(getattr(h, "conv") is time.localtime)
print(getattr(Child, "log") is math.log)

# Instance-dict shadowing stays a plain value read.
h2 = Holder()
h2.conv = time.gmtime
print(h2.conv is time.gmtime)
print(h2.conv(0)[:6])
print(h.conv is time.localtime)

# The stdlib pattern this pins: a converter stored as a class attribute is
# called with exactly the arguments written at the call site.
class Stamper:
    converter = time.gmtime

    def stamp(self, seconds):
        return self.converter(seconds)[:6]


print(Stamper().stamp(0))
print(Stamper().stamp(86400))
