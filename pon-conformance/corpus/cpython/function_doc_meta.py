# Function dunder metadata defaults: __doc__ / __module__ / __qualname__ are
# readable on plain functions (enum.py and functools-family code probe them,
# e.g. `if func.__doc__ is None:`), and a stored value wins over the default
# because the per-function __dict__ is consulted first.
# Deliberately excluded (known pon gaps, diverge from CPython today):
# literal-docstring extraction (`def f(): "text"` reports None), nested
# `__qualname__` (`outer.<locals>.inner` prefix), and bare `del f.__doc__`
# without a prior store.

def f():
    pass

# Defaults on a plain function.
print(f.__doc__)
print(f.__module__)
print(f.__name__)
print(f.__qualname__)
print(hasattr(f, "__doc__"))
print(hasattr(f, "__module__"))
print(getattr(f, "__doc__", "fallback"))

# `is None` probe shape used by enum's doc-fixup paths.
print(f.__doc__ is None)

# Stored __doc__ wins over the default, then delete falls back to None.
f.__doc__ = "written later"
print(f.__doc__)
print(f.__doc__ is None)
del f.__doc__
print(f.__doc__)

# Explicit None store round-trips.
f.__doc__ = None
print(f.__doc__)

# __module__ is writable the same way.
f.__module__ = "custom.namespace"
print(f.__module__)

# Lambdas report the same None default.
double = lambda x: x * 2
print(double.__doc__)

# Functions with defaults/closures take the full construction path; the
# metadata surface is identical.
def outer(base, scale=2):
    def inner():
        return base * scale
    return inner

print(outer.__doc__)
print(outer.__module__)
inner = outer(3)
print(inner.__doc__)
print(inner())

# The manual functools.wraps mechanic copies __doc__ across functions.
def wrapped():
    return "payload"

wrapped.__doc__ = "wrapped docs"

def wrapper():
    return wrapped()

wrapper.__doc__ = wrapped.__doc__
print(wrapper.__doc__)
print(wrapper())
