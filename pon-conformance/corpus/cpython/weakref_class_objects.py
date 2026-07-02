# Weak references to class objects: user (heap) classes and builtin types.
#
# Collection behavior is intentionally untested here: pon class objects are
# immortal (leaked type boxes), so a class weakref never clears, while CPython
# collects unreferenced heap classes and fires their weakref callbacks. Any
# collection-dependent assertion would diverge, so this module covers live
# referent semantics and error paths only. (Verified against CPython 3.14:
# no class weakref callback fires at interpreter shutdown for module-level
# classes, so the live-callback leg below is deterministic on both sides.)
import weakref


class C:
    pass


class D:
    pass


# Live deref and identity for a user (heap) class.
r = weakref.ref(C)
print("class live", r() is C)

# The dereffed class object is fully usable: construct an instance through it.
obj = r()()
print("class construct", isinstance(obj, C), isinstance(obj, D))

# Weak references to builtin (static) type objects are legal in CPython 3.14.
ri = weakref.ref(int)
print("builtin live", ri() is int)

# __callback__ introspection: absent -> None; a live callback never fires.
cb = weakref.ref(D, lambda wr: print("unexpected callback"))
print("callback attr", r.__callback__ is None, cb.__callback__ is None)
print("callback live", cb() is D)

# Equality follows live referents; hash follows the referent's hash.
r2 = weakref.ref(C)
rd = weakref.ref(D)
print("eq", r == r2, r == rd)
print("hash", hash(r) == hash(C), hash(rd) == hash(D))

# Non-weakrefable instances are still rejected with TypeError.
for bad in (42, [1, 2]):
    try:
        weakref.ref(bad)
        print("no error")
    except TypeError as exc:
        print("TypeError:", exc)

# WeakSet holds classes (the _py_abc registry pattern): add/contains/discard.
from _weakrefset import WeakSet

ws = WeakSet()
ws.add(C)
ws.add(D)
print("weakset add", C in ws, D in ws, int in ws)
ws.discard(C)
print("weakset discard", C in ws, D in ws)
