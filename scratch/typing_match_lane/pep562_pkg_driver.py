from pep562_pkg import Lazy
print('Lazy=', Lazy)
from pep562_pkg import child
print('child=', child.value)
try:
    from pep562_pkg import Missing
except ImportError as e:
    print(type(e).__name__, str(e))
