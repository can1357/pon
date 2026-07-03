# PEP 420 namespace packages: a directory without __init__.py imports as a
# namespace package; portions on distinct search-path roots compose into one
# __path__; a real module or regular package on ANY root beats a bare
# directory recorded on an earlier root.
#
# The package tree is created at runtime under a pid-unique /tmp directory so
# the corpus stays self-contained and parallel-safe.  Both engines make the
# roots importable through their own mechanism: CPython via sys.path, pon via
# os.putenv("PONPATH", ...) which writes through to the process environment
# the import resolver re-reads per import.  Printed paths are normalized to
# "@" so output is byte-identical across runs.
import os
import sys

# CPython would otherwise drop __pycache__ dirs into the created tree and
# break the exact-cleanup accounting below; pon writes no bytecode.
sys.dont_write_bytecode = True

TD = "/tmp/pon_nspkg_%d" % os.getpid()
R1 = TD + "/r1"
R2 = TD + "/r2"

DIRS = [
    TD,
    R1,
    R1 + "/ns_pkg",
    R1 + "/ns_pkg/deep",
    R1 + "/ns_pkg/reg",
    R1 + "/shadow",
    R2,
    R2 + "/ns_pkg",
    R2 + "/ns_pkg/deep",
]
FILES = [
    (R1 + "/ns_pkg/alpha.py", "A = 'alpha-r1'\n"),
    (R1 + "/ns_pkg/deep/leaf.py", "L = 'leaf-r1'\n"),
    (R1 + "/ns_pkg/reg/__init__.py", "TAG = 'reg-pkg'\n"),
    (R1 + "/ns_pkg/reg/inner.py", "V = 'inner'\n"),
    (R2 + "/ns_pkg/beta.py", "B = 'beta-r2'\n"),
    (R2 + "/ns_pkg/deep/leaf2.py", "L2 = 'leaf2-r2'\n"),
    (R2 + "/shadow.py", "SH = 'module-wins'\n"),
]


def rel(path):
    return path.replace(TD, "@")


for d in DIRS:
    os.mkdir(d)
for path, text in FILES:
    with open(path, "w") as handle:
        handle.write(text)

# CPython resolves through sys.path; pon ignores sys.path but re-reads the
# PONPATH process environment variable on every import.  Both mechanisms are
# applied unconditionally and each engine consumes the one it understands.
paths = getattr(sys, "path", None)
if paths is not None:
    paths.insert(0, R2)
    paths.insert(0, R1)
os.putenv("PONPATH", R1 + os.pathsep + R2)

try:
    import ns_pkg

    print("name:", ns_pkg.__name__)
    print("package:", ns_pkg.__package__)
    print("file present:", hasattr(ns_pkg, "__file__"))
    print("file is None:", getattr(ns_pkg, "__file__", "<absent>") is None)
    print("path:", [rel(p) for p in list(ns_pkg.__path__)])

    import ns_pkg.alpha
    import ns_pkg.beta

    print("alpha:", ns_pkg.alpha.A)
    print("beta:", ns_pkg.beta.B)
    print("alpha package:", ns_pkg.alpha.__package__)

    import ns_pkg.deep.leaf
    import ns_pkg.deep.leaf2

    print("deep path:", [rel(p) for p in list(ns_pkg.deep.__path__)])
    print("deep file is None:", getattr(ns_pkg.deep, "__file__", "<absent>") is None)
    print("leaf:", ns_pkg.deep.leaf.L)
    print("leaf2:", ns_pkg.deep.leaf2.L2)

    from ns_pkg.reg import inner

    print("reg tag:", ns_pkg.reg.TAG)
    print("reg file:", os.path.basename(ns_pkg.reg.__file__))
    print("inner:", inner.V)

    import shadow

    print("shadow:", shadow.SH)
    print("shadow is plain module:", not hasattr(shadow, "__path__"))

    try:
        import ns_pkg.missing
    except ModuleNotFoundError as error:
        print("missing:", str(error))
finally:
    for path, _ in FILES:
        os.unlink(path)
    for d in reversed(DIRS):
        os.rmdir(d)

print("cleaned:", not os.path.exists(TD))
