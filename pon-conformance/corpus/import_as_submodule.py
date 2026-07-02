import pkg.sub as s
print(s.__name__)
print(s.sub_result)
try:
    pkg
except NameError:
    print("pkg unbound after as-import")
import pkg.sub.leaf as l
print(l.__name__)
print(l.leaf_result)
import pkg.sub
print(pkg.__name__)
print(pkg.init_result)
print(pkg.sub.__name__)
print(pkg.sub.leaf.leaf_result)
print(s is pkg.sub)
print(l is pkg.sub.leaf)
import pkg as p
print(p.__name__)
print(p.x)
