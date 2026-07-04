import cython
cleanup_level_for_type_prefix = cython.declare(object, {
    'ustring': None,
    'tuple': 2,
    'slice': 2,
}.get)
print(cleanup_level_for_type_prefix('tuple'))
