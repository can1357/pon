from _py_abc import ABCMeta
print("in dict", "_abc_invalidation_counter" in ABCMeta.__dict__)
print(ABCMeta._abc_invalidation_counter)
