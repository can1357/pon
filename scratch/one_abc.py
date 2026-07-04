import abc
print(hasattr(abc, "ABCMeta"), getattr(getattr(abc, "ABCMeta", None), "__name__", None))
