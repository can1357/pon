# Module-level __getattr__ for attribute access and from-import.
import os
import sys

sys.dont_write_bytecode = True

ROOT = "/tmp/pon_pep562_module_getattr_%d" % os.getpid()
PKG = ROOT + "/lazy_pkg"
FILES = [
    (
        PKG + "/__init__.py",
        "present = 'ready'\n"
        "def __getattr__(name):\n"
        "    if name == 'lazy':\n"
        "        return 'value-' + name\n"
        "    if name == 'broken':\n"
        "        raise AttributeError('missing-' + name)\n"
        "    raise AttributeError(name)\n",
    ),
]

os.mkdir(ROOT)
os.mkdir(PKG)
for path, text in FILES:
    with open(path, "w") as handle:
        handle.write(text)

old_ponpath = os.getenv("PONPATH")
sys.path.insert(0, ROOT)
os.putenv("PONPATH", ROOT + ((":" + old_ponpath) if old_ponpath else ""))

try:
    import lazy_pkg

    print("plain", lazy_pkg.lazy)
    from lazy_pkg import lazy

    print("from", lazy)
    try:
        from lazy_pkg import broken
    except ImportError as exc:
        print("broken", type(exc).__name__)

    from typing import Match

    print(Match)
finally:
    if sys.path and sys.path[0] == ROOT:
        del sys.path[0]
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
