descr = type.__dict__["__annotations__"]
print(type(descr).__name__)
print(descr.__name__)

# annotationlib captures the unbound getter at module scope and calls it
# later with (cls,) — the bound method must survive being stored.
get_annotations = descr.__get__


class Annotated:
    x: int
    y: str = "start"


class Plain:
    pass


print(get_annotations(Annotated))
print(get_annotations(Plain))
print(get_annotations(Annotated) is Annotated.__annotations__)

# Class-level read/write round-trip through the data descriptor.
class Mutable:
    pass


Mutable.__annotations__ = {"z": float}
print(Mutable.__annotations__)
print(get_annotations(Mutable))
del Mutable.__annotations__
print(get_annotations(Mutable))

# Static-type parity: the descriptor's home raises AttributeError.
try:
    get_annotations(type)
except AttributeError:
    print("static type raises AttributeError")

# The receiver must be a class.
try:
    get_annotations(3)
except TypeError:
    print("non-class receiver raises TypeError")

import annotationlib

print(annotationlib.get_annotations(Annotated))
print(annotationlib.get_annotations(Plain))
print("annotationlib ok")
