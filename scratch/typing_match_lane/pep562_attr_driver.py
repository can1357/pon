import pep562_mod
print(pep562_mod.Lazy)
try:
    print(pep562_mod.Missing)
except AttributeError as e:
    print(type(e).__name__, str(e))
