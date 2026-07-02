mro_descr = type.__dict__["__mro__"]
dict_descr = type.__dict__["__dict__"]
ann_descr = type.__dict__["__annotations__"]
print(type(mro_descr).__name__, type(dict_descr).__name__, type(ann_descr).__name__)
print(mro_descr.__name__, dict_descr.__name__, ann_descr.__name__)
print(repr(mro_descr))
print(repr(dict_descr))
print(mro_descr.__qualname__, dict_descr.__qualname__)

# inspect.py captures both unbound getters at module scope (lines 1667/1668:
# `_static_getmro` / `_get_dunder_dict_of_class`) and calls them much later;
# annotationlib does the same with `__annotations__`.  The bound methods must
# survive being stored.
_static_getmro = mro_descr.__get__
_get_dunder_dict_of_class = dict_descr.__get__
_get_annotations = ann_descr.__get__


class Base:
    marker = "base"
    tagged: int = 3


class Sub(Base):
    pass


# --- __mro__: tuple parity with the attribute read, static terminus. -------
print(type(_static_getmro(Sub)) is tuple)
print([entry.__name__ for entry in _static_getmro(Sub)])
print([entry.__name__ for entry in _static_getmro(type)])
print(_static_getmro(Sub) == Sub.__mro__)
print(_static_getmro(object) == (object,))

# --- __dict__: own-namespace reads, no MRO inheritance. --------------------
base_ns = _get_dunder_dict_of_class(Base)
print("marker" in base_ns, base_ns["marker"], base_ns["tagged"])
print("marker" in _get_dunder_dict_of_class(Sub))

# --- __annotations__: same capture pattern, own-class storage. -------------
print(_get_annotations(Base))
print(_get_annotations(Sub))
try:
    _get_annotations(type)
except AttributeError as e:
    print("static type:", e)

# --- Read-only data descriptors (unlike writable __annotations__). ---------
try:
    Sub.__mro__ = ()
except AttributeError as e:
    print("set attr:", e)
try:
    del Sub.__mro__
except AttributeError as e:
    print("del attr:", e)
try:
    mro_descr.__set__(Sub, ())
except AttributeError as e:
    print("descr set:", e)
try:
    Sub.__dict__ = {}
except AttributeError as e:
    print("dict set attr:", e)
try:
    dict_descr.__set__(Sub, {})
except AttributeError as e:
    print("dict descr set:", e)
Sub.__annotations__ = {"z": float}
print(_get_annotations(Sub))

# --- Non-class receivers are rejected. --------------------------------------
try:
    _static_getmro(3)
except TypeError as e:
    print("mro non-class:", e)
try:
    _get_dunder_dict_of_class("x")
except TypeError as e:
    print("dict non-class:", e)

# --- One shared getset_descriptor type (types.GetSetDescriptorType). -------
# inspect's `_shadowed_dict` recognizes the legitimate `__dict__` getset by
# type identity + `__objclass__`; both checks must hold or `getattr_static`
# skips every MRO entry.
import types

print(type(mro_descr) is types.GetSetDescriptorType)
print(type(dict_descr) is types.GetSetDescriptorType)
print(type(ann_descr) is type(types.FunctionType.__code__))
print(mro_descr.__objclass__ is type, dict_descr.__objclass__ is type)
print(types.FunctionType.__code__.__objclass__ is types.FunctionType)
print(types.FunctionType.__code__.__name__)
print(repr(types.FunctionType.__code__))

# Probing a getset descriptor for an attribute it lacks must fall back
# cleanly (this used to be a hard crash, not an AttributeError).
print(getattr(dict_descr, "__no_such_attr__", "MISSING"))
print(hasattr(mro_descr, "__objclass__"), hasattr(mro_descr, "__no_such_attr__"))

# --- The consumer that gated everything: inspect's static introspection. ---
import inspect

print(inspect.isclass(Sub), inspect.isclass(Sub()))
print(inspect.getmro(Sub) == Sub.__mro__)
print(inspect.getattr_static(Sub(), "marker"))
print(inspect.getattr_static(Sub, "tagged"))
print("inspect ok")
