# Relative imports inside functions after package import completion.
import os
import sys

sys.dont_write_bytecode = True

ROOT = "/tmp/pon_relative_import_function_scope_%d" % os.getpid()
PKG = ROOT + "/relpkg_case"

os.mkdir(ROOT)
os.mkdir(PKG)

FILES = [
    (
        PKG + "/__init__.py",
        "INIT_NAME = __name__\n"
        "INIT_PACKAGE = __package__\n",
    ),
    (
        PKG + "/sibling.py",
        "NAME = 'sibling-value'\n"
        "MODULE_NAME = __name__\n"
        "MODULE_PACKAGE = __package__\n",
    ),
    (
        PKG + "/worker.py",
        "from . import sibling as top_sibling\n"
        "TOP_VALUE = top_sibling.NAME\n"
        "def import_sibling_in_function():\n"
        "    from . import sibling\n"
        "    return sibling.NAME, sibling.__name__, sibling.__package__\n"
        "def import_name_in_function():\n"
        "    from .sibling import NAME\n"
        "    return NAME\n",
    ),
]

for path, text in FILES:
    with open(path, "w") as handle:
        handle.write(text)

sys.path.insert(0, ROOT)
os.putenv("PONPATH", ROOT)

try:
    import relpkg_case
    from relpkg_case import worker

    print("package", relpkg_case.__name__, relpkg_case.__package__)
    print("worker", worker.__name__, worker.__package__)
    print("top", worker.TOP_VALUE, worker.top_sibling.__name__, worker.top_sibling.__package__)
    print("after import complete")
    value, mod_name, mod_package = worker.import_sibling_in_function()
    print("function module", value, mod_name, mod_package)
    print("function name", worker.import_name_in_function())
finally:
    for path, _text in FILES:
        try:
            os.remove(path)
        except OSError:
            pass
    try:
        os.rmdir(PKG)
    except OSError:
        pass
    try:
        os.rmdir(ROOT)
    except OSError:
        pass
