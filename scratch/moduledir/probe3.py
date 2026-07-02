import pkg.sib as sib

# vars(module) mutation must flow back into module attrs (live dict)
vars(sib)["via_dict"] = 41
print("via_dict" in dir(sib), getattr(sib, "via_dict", "MISSING"))

# module attr set/del must be reflected by dir()
sib.extra_attr = 1
print("extra_attr" in dir(sib))
del sib.extra_attr
print("extra_attr" in dir(sib))

# dir(pkg) sees submodule binding
import pkg.sub
print("sub" in dir(pkg))

# dir result is a sorted, duplicate-free list mirroring __dict__
d = dir(sib)
print(type(d) is list, d == sorted(d), len(d) == len(set(d)))
print(sorted(set(d) - set(vars(sib).keys())))
print(all(hasattr(sib, n) for n in d))
