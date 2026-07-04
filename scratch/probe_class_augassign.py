from _py_abc import ABCMeta
print(ABCMeta._abc_invalidation_counter)
ABCMeta._abc_invalidation_counter += 1
print(ABCMeta._abc_invalidation_counter)
