# Derived from CPython v3.14.0 Lib/importlib/_bootstrap.py `_find_and_load`
# semantics (PSF license): a `None` binding in `sys.modules` is a deliberate
# import block (`test.support.import_helper.import_fresh_module` plants one
# per blocked name).
#
# Every import form must halt with ModuleNotFoundError -- catchable as plain
# ImportError, the fallback contract each pure-Python stdlib accelerator
# wrapper relies on (bisect/queue/stat/collections/decimal) -- while any
# non-None binding is served verbatim, and deleting the binding restores
# normal importability.

import importlib
import sys

sys.modules.pop('stat', None)
sys.modules['stat'] = None

# --- plain import halts -------------------------------------------------
try:
    import stat
except ModuleNotFoundError as exc:
    print("plain halt:", exc)

# --- catchable as the ImportError base (accelerator-fallback contract) ---
try:
    import stat
except ImportError as exc:
    print("as ImportError:", type(exc).__name__)

# --- from-import halts ---------------------------------------------------
try:
    from stat import S_IMODE
except ImportError as exc:
    print("from halt:", type(exc).__name__, exc)

# --- star-import halts (module scope, the bisect.py shape) ---------------
try:
    from stat import *
except ImportError as exc:
    print("star halt:", type(exc).__name__, exc)

# --- importlib.import_module halts (the import_fresh_module shape) -------
try:
    importlib.import_module('stat')
except ImportError as exc:
    print("importlib halt:", type(exc).__name__, exc)

# --- only None blocks: any other binding is served verbatim --------------
sys.modules['stat'] = 'sentinel, not a module'
import stat
print("non-None binding:", stat)

# --- deleting the block restores importability ----------------------------
del sys.modules['stat']
import stat
print("restored:", stat.__name__, stat.S_IFDIR, stat.filemode(0o100644))

# --- fresh-import dance: pop, block the accelerator, reimport pure-Py -----
saved = {}
for name in ('bisect', '_bisect'):
    if name in sys.modules:
        saved[name] = sys.modules.pop(name)
sys.modules['_bisect'] = None
try:
    fresh = importlib.import_module('bisect')
    print("fresh bisect:", fresh.__name__, fresh.bisect_right([1, 2, 2, 3], 2))
    values = [1, 3]
    fresh.insort(values, 2)
    print("insort works:", values)
finally:
    for name in ('bisect', '_bisect'):
        sys.modules.pop(name, None)
    sys.modules.update(saved)
print("done")
