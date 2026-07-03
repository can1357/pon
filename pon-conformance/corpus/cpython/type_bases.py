# `cls.__bases__` — the declared direct bases across every class-construction
# kind — plus `__mro__` ATTRIBUTE reads (not just `type.__dict__['__mro__']`).
#
# Deliberately absent (pon divergences documented at the descriptor in
# descr.rs): `C.__bases__ = ...` assignment / deletion (CPython supports live
# re-basing; pon raises the read-only getset AttributeError), and deep
# exception-MRO chains (exception linearization is its own surface).
# `bool` linearizes through `int` like CPython; bool_int_base.py owns that
# matrix.

# --- builtin/static types; object is the empty-bases terminus. --------------
print(int.__bases__)
print(object.__bases__)
print(type.__bases__)
print(int.__mro__)

# --- plain heap classes: implicit object, explicit single, multi-base. ------
class A: pass
class B: pass
class C(A, B): pass

print(A.__bases__ == (object,))
print(C.__bases__ == (A, B))
print(C.__mro__ == (C, A, B, object))

# --- three-argument type(): the construction record survives. ---------------
X = type('X', (int,), {})
print(X.__bases__)
print(X.__mro__ == (X, int, object))
print(type('Y', (), {}).__bases__)

# --- metaclasses and metaclass-made classes. ---------------------------------
class Meta(type): pass
print(Meta.__bases__)
print(Meta.__mro__ == (Meta, type, object))
M = Meta('M', (A,), {})
print(M.__bases__ == (A,))
print(M.__mro__ == (M, A, object))
print(type(M) is Meta)

# --- native-layout and exception construction paths keep the record. --------
class D(dict): pass
class L(list): pass
class T(tuple): pass
class E(ValueError): pass
class S:
    __slots__ = ('a',)

print(D.__bases__ == (dict,), L.__bases__ == (list,), T.__bases__ == (tuple,))
print(E.__bases__ == (ValueError,))
print(S.__bases__ == (object,))

# --- the type.__dict__ getset agrees with the attribute surface. ------------
descr = type.__dict__['__bases__']
print(repr(descr))
print(descr.__name__, descr.__objclass__ is type)
print(descr.__get__(C) == C.__bases__)
import types
print(type(descr) is types.GetSetDescriptorType)

# --- instances never see the class-only surface. -----------------------------
print(hasattr(A(), '__bases__'), hasattr(A, '__bases__'))
print(getattr(A, '__bases__') == (object,))
