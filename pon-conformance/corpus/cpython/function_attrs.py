# Function objects back arbitrary attribute get/set/del with a per-function
# __dict__ (CPython funcobject semantics): the storage abc.abstractmethod and
# functools.wraps rely on.
# functools.wraps itself is exercised manually below: `import functools` is
# blocked in pon by an unrelated closure-lowering gap (cmp_to_key's `mycmp`).
from abc import abstractmethod

def f():
    return 1

# Arbitrary attribute set/get across value kinds.
f.tag = 7
f.label = "probe"
f.stack = [1, 2]
print(f.tag)
print(f.label)
print(f.stack)
f.tag = 8
print(f.tag)

# getattr default shape before/after, hasattr before/after.
print(getattr(f, "missing", "fallback"))
print(getattr(f, "tag", "fallback"))
print(hasattr(f, "extra"))
f.extra = True
print(hasattr(f, "extra"))

# Deletion removes the attribute; deleting again raises AttributeError.
del f.extra
print(hasattr(f, "extra"))
try:
    del f.extra
except AttributeError:
    print("del-missing-attributeerror")

# __dict__ reflects plain stores and supports direct mutation.
print(sorted(f.__dict__))
f.__dict__["injected"] = "via-dict"
print(f.injected)

# abc.abstractmethod round-trip: the exact ABCMeta probe shape.
def g():
    return 2

print(getattr(g, "__isabstractmethod__", False))
abstractmethod(g)
print(g.__isabstractmethod__)
print(getattr(g, "__isabstractmethod__", False))
print(g())

# Nested and closure functions carry their own attribute namespaces.
def outer(base):
    def inner():
        return base
    inner.kind = "closure"
    return inner

first = outer(10)
second = outer(20)
first.kind = "first"
print(first.kind)
print(second.kind)
print(first())
print(second())

# Lambdas are plain functions for attribute purposes.
double = lambda x: x * 2
double.doc_tag = "lambda-attr"
print(double.doc_tag)
print(double(4))

# The functools.wraps mechanic, spelled manually: copy metadata attributes,
# merge __dict__, and record __wrapped__.
def wrapped():
    return "payload"

wrapped.marker = "wrapped-marker"

def wrapper():
    return wrapped()

wrapper.__dict__.update(wrapped.__dict__)
wrapper.__wrapped__ = wrapped
print(wrapper.marker)
print(wrapper.__wrapped__ is wrapped)
print(sorted(wrapper.__dict__))
print(wrapper())
