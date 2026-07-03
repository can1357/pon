# importlib.import_module resolves source modules and packages through the
# same machinery as the import statement (pon: the _pon_source_importer
# meta-path finder standing in CPython's PathFinder slot; identical
# observable results, so this file prints the same lines on both sides).

import importlib
import sys

# Source package, first imported through importlib (no prior statement import).
json_mod = importlib.import_module("json")
print(json_mod.__name__)
print(json_mod.__package__)
print(json_mod is sys.modules["json"])
print(json_mod.__spec__.name)

# A statement import afterwards binds the same module object.
import json
print(json is json_mod)

# Dotted submodule of a source package, and the parent attribute binding.
decoder = importlib.import_module("json.decoder")
print(decoder.__name__)
print(decoder is sys.modules["json.decoder"])
print(json.decoder is decoder)

# Plain source module (not a package).
colorsys = importlib.import_module("colorsys")
print(colorsys.__name__)
print(colorsys.__package__ == "")
print(len(colorsys.rgb_to_hsv(0.5, 0.5, 0.5)))

# Builtin/native modules keep resolving through the earlier finders.
math_mod = importlib.import_module("math")
print(math_mod.__name__)
print(math_mod is sys.modules["math"])

# A missing name raises CPython's ModuleNotFoundError, message included.
try:
    importlib.import_module("pon_no_such_module_xyz")
except ModuleNotFoundError as exc:
    print(type(exc).__name__)
    print(exc)

# The loaded package is genuinely usable.
print(json.dumps({"k": [1, 2]}))
