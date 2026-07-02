# dir(module) / vars(module) / module.__dict__ surface over the read-only
# corpus `pkg` package (runtime-only mutation; no fixture is edited).
#
# Documented divergence (not asserted): CPython source modules also carry
# __builtins__ / __cached__ / __doc__ / __loader__ / __spec__ in their
# namespace, and __file__ exists only under pon's JIT source loader (not for
# AoT-embedded bodies). pon serves __name__ / __package__ (+ __file__ via the
# source loader), so full dir() equality is unreachable; the assertions below
# cover user-defined names, membership, ordering, and dict identity instead.
import pkg
import pkg.sib as sib

print(sorted(n for n in dir(sib) if not n.startswith("_")))
print(sorted(n for n in dir(pkg) if not n.startswith("_")))
print("__name__" in dir(sib), "__package__" in dir(sib))

d = dir(sib)
print(type(d) is list, d == sorted(d), len(d) == len(set(d)))

ns = vars(sib)
print(ns is sib.__dict__, type(ns) is dict)
print(ns["x"], ns["sib_name"], ns["sib_package"])
print(all(hasattr(sib, n) for n in d))
print(sorted(set(d) - set(ns.keys())))

# dir() reflects post-import mutation through both setattr spellings.
sib.marker_stmt = "stmt"
setattr(sib, "marker_call", "call")
print("marker_stmt" in dir(sib), "marker_call" in dir(sib))
print(sib.marker_stmt, getattr(sib, "marker_call"))
del sib.marker_stmt
delattr(sib, "marker_call")
print("marker_stmt" in dir(sib), "marker_call" in dir(sib))

# The namespace view is live: writes through vars() resolve as attributes.
vars(sib)["via_dict"] = 41
print("via_dict" in dir(sib), sib.via_dict)

# The same module reached through sys.modules serves the same namespace.
import sys

print(vars(sys.modules["pkg.sib"]) is vars(sib))

# Submodule binding appears on the parent package only once imported.
print("sub" in dir(pkg))
import pkg.sub

print("sub" in dir(pkg))
print(sorted(n for n in dir(pkg.sub) if not n.startswith("_")))
