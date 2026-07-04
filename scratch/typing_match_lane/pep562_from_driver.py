from pep562_mod import Lazy
print(Lazy)
try:
    from pep562_mod import Missing
except ImportError as e:
    print(type(e).__name__, str(e))
