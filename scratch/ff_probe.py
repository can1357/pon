import sys
print(float.__getformat__("double"))
print(float.__getformat__("float"))
try:
    float.__getformat__("x")
except ValueError as e:
    print("ValueError:", e)
try:
    float.__getformat__(1)
except TypeError as e:
    print("TypeError:", e)
try:
    float.__getformat__()
except TypeError as e:
    print("TypeError:", e)
try:
    float.__getformat__("a", "b")
except TypeError as e:
    print("TypeError:", e)
i = sys.implementation
print(i.name, i.cache_tag, i.hexversion)
print(i.version is sys.version_info)
print(type(i).__name__)
print(hasattr(i, "_multiarch"), getattr(i, "_multiarch", "<absent>"))
print(repr(i))
import types
print(types.SimpleNamespace is type(i))
