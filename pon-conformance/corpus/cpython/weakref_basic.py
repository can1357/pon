import gc
import weakref


class Box:
    pass


obj = Box()
ref = weakref.ref(obj)
other = weakref.ref(obj)
print("live", ref() is obj)
print("callback attr", ref.__callback__ is None, other.__callback__ is None)
print("eq live", ref == other, ref == ref)
del obj
gc.collect()
gc.collect()
print("dead", ref() is None, other() is None)
