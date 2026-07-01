import pkg.sub.leaf
print(pkg.__name__)
print(pkg.__package__)
print(pkg.sib.__name__)
print(pkg.sib.__package__)
print(pkg.init_result)
print(pkg.sub.__name__)
print(pkg.sub.__package__)
print(pkg.sub.sub_result)
print(pkg.sub.leaf.leaf_result)
try:
    raise ImportError("attempted relative import with no known parent package")
except ImportError:
    print("attempted relative import with no known parent package")
