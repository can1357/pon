m = {}.get
print(type(m))
print(type(m).__name__)
print(getattr(type(m), '__mro__', None))
print(isinstance(m, object))
