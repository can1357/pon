# sys._jit introspection surface: deterministic reads only.
# CPython 3.14 ships sys._jit as a module introspecting ITS OWN experimental
# tier-2 JIT; pon serves an opaque singleton (the flags/hash_info pattern),
# so type(sys._jit) is NOT asserted (module vs. sys._jit). pon has a JIT
# (tier-up), but it is not the JIT this surface probes: is_enabled() is
# honestly False, matching the non-JIT host oracle build test.support gates
# on at import (requires_jit_enabled/_disabled skip markers). Only the
# consumed subset is served: is_available/is_active exist on the CPython
# module but stay unserved in pon until a walk consumes them, so they are
# not asserted either.
import sys

result = sys._jit.is_enabled()
print(result)
print(type(result).__name__)
print(result is False)

# The accessor survives repeated attribute walks; identity of the function
# object is NOT asserted (module function vs. native function).
print(sys._jit.is_enabled() == sys._jit.is_enabled())
