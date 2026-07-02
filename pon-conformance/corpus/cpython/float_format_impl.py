# float.__getformat__ is a classmethod on the float type object; CPython
# keeps it for test.support's import-time requires_IEEE_754 gate
# (float.__getformat__("double").startswith("IEEE"), support/__init__.py:510).
# Rust doubles/singles are IEEE 754 by definition, so both interpreters print
# the same format string on this platform, and the rejection legs print the
# CPython 3.14 message texts.
#
# sys.implementation is checked SHAPE-ONLY: pon's honest .name/.cache_tag
# values ('pon'/'pon-314') diverge from the oracle ('cpython'/'cpython-314')
# by design (documented in native/sys.rs), so only attribute existence, field
# types, derived invariants, and the version identity are printed here.

import sys
import types

# The test.support:510 gate expression itself.
print(float.__getformat__("double"))
print(float.__getformat__("float"))
print(float.__getformat__("double").startswith("IEEE"))
print(float.__getformat__("double") == float.__getformat__("float"))

# Rejection legs: ValueError for an unknown format name, TypeError for a
# non-str argument and for wrong arities.
try:
    float.__getformat__("long double")
except ValueError as exc:
    print("ValueError:", exc)
try:
    float.__getformat__(1)
except TypeError as exc:
    print("TypeError:", exc)
try:
    float.__getformat__()
except TypeError as exc:
    print("TypeError:", exc)
try:
    float.__getformat__("double", "float")
except TypeError as exc:
    print("TypeError:", exc)

# sys.implementation: SimpleNamespace shape over the PEP 421 required fields.
impl = sys.implementation
print(hasattr(impl, "name"), hasattr(impl, "cache_tag"))
print(hasattr(impl, "version"), hasattr(impl, "hexversion"))
print(type(impl.name).__name__, type(impl.cache_tag).__name__, type(impl.hexversion).__name__)
print(type(impl).__name__)
print(types.SimpleNamespace is type(impl))

# The version field is THE sys.version_info structseq singleton, and the
# cache_tag/hexversion derive from the same version pin.
print(impl.version is sys.version_info)
print(impl.version == sys.version_info)
print(impl.cache_tag.endswith("-%d%d" % (sys.version_info[0], sys.version_info[1])))
print(impl.hexversion == sys.hexversion)
