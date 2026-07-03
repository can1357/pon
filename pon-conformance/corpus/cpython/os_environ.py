import os

# pon's os.environ is a plain str->str dict snapshot (documented decision in
# native/os.rs environ_snapshot: no _Environ write-through, since nothing in
# the cohort can observe the real environment from Python).  Everything below
# is the CPython-parity subset: dict-level mutation is visible through
# os.getenv (which reads the LIVE os.environ binding on both engines), while
# putenv/unsetenv write the real process environment and deliberately do NOT
# touch os.environ — identical on CPython, where _Environ.__setitem__ calls
# putenv and never the reverse.

K = 'PON_CORPUS_ENV_KEY'
print(K in os.environ, os.getenv(K), os.getenv(K, 'dflt'))

os.environ[K] = 'v1'
print(os.environ[K], os.getenv(K), K in os.environ)
print(os.environ.get(K), os.environ.get(K + '_MISSING'), os.environ.get(K + '_MISSING', 'd'))

# setdefault: existing key keeps its value, missing key inserts.
print(os.environ.setdefault(K, 'v2'))
print(os.environ.setdefault(K + '2', 'v3'), os.getenv(K + '2'))

del os.environ[K + '2']
print(os.getenv(K + '2', 'gone'))

print(os.environ.pop(K))
print(os.getenv(K), K in os.environ)

# putenv/unsetenv write through to the real environment only: os.environ and
# getenv never see them (CPython docs: putenv does not update os.environ).
os.putenv('PON_CORPUS_PUTENV_KEY', 'real-value')
print(os.getenv('PON_CORPUS_PUTENV_KEY'), 'PON_CORPUS_PUTENV_KEY' in os.environ)
os.unsetenv('PON_CORPUS_PUTENV_KEY')
print('unsetenv ok')

# Exact CPython argument errors.
for args in [('A=B', 'x'), ('A\x00B', 'x'), ('A', 'x\x00y')]:
    try:
        os.putenv(*args)
    except ValueError as exc:
        print('ValueError:', exc)
try:
    os.putenv(1, 'x')
except TypeError as exc:
    print('TypeError:', exc)
try:
    os.unsetenv(2)
except TypeError as exc:
    print('TypeError:', exc)
try:
    os.unsetenv('A=B')
except OSError as exc:
    print('OSError:', exc)
try:
    os.unsetenv('A\x00B')
except ValueError as exc:
    print('ValueError:', exc)
try:
    os.getenv(3)
except TypeError as exc:
    print('TypeError:', exc)

# getenv reads the LIVE os.environ module binding — the same os.py
# global-read semantics test.support's EnvironmentVarGuard.__exit__ relies
# on when it rebinds os.environ during restore.
saved = os.environ
os.environ = {'PON_CORPUS_REBOUND': 'yes'}
print(os.getenv('PON_CORPUS_REBOUND'), os.getenv('PATH') is None)
os.environ = saved
print(os.getenv('PON_CORPUS_REBOUND') is None)

print(hasattr(os, 'putenv'), hasattr(os, 'unsetenv'), hasattr(os, 'getenv'))
import posix
print(hasattr(posix, 'putenv'), hasattr(posix, 'unsetenv'), hasattr(posix, 'getenv'))
