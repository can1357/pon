# sys import-protocol surface: deterministic reads only.
# Contents of sys.meta_path are NOT asserted: CPython serves
# [BuiltinImporter, FrozenImporter, PathFinder] from the frozen bootstrap,
# while pon seeds [BuiltinImporter, FrozenImporter] lazily when
# importlib._bootstrap first loads (PathFinder needs _bootstrap_external's
# file machinery). Only the type, mutability, and platlibdir value are
# oracle-stable.
import sys

print(type(sys.meta_path).__name__)
print(type(sys.warnoptions).__name__)

before = len(sys.meta_path)
sentinel = object()
sys.meta_path.append(sentinel)
print(len(sys.meta_path) - before)
print(sys.meta_path[-1] is sentinel)
removed = sys.meta_path.pop()
print(removed is sentinel)
print(len(sys.meta_path) == before)

print(type(sys.platlibdir).__name__)
print(sys.platlibdir)
