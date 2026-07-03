# sys constant/structseq surface: float_info / int_info / thread_info are
# structseqs (tuple subclasses with named read-only fields) whose reprs are
# printed whole — the differential oracle is the host python3.14, and pon
# renders elements through its own repr dispatch, so float shortest-repr,
# str quoting, and None must agree byte-for-byte.  sys.getsizeof sizes are
# IMPLEMENTATION-DEFINED (CPython layout bytes vs pon allocation bytes):
# only int-ness/positivity is printed, never numeric sizes.  The audit pair
# is exercised through a prefix-filtered hook because the oracle fires
# built-in events into registered hooks (import/open/...) that pon — which
# raises no built-in events — never sends; filtering pins the shared
# user-visible contract.  flags.dont_write_bytecode/hash_randomization
# (env-derived) and stdin isatty/seekable (tty-vs-pipe) are deliberately
# unprinted.

import sys

# float_info: repr, field/index equivalence, tuple protocol.
fi = sys.float_info
print(repr(fi))
print(type(fi).__name__, isinstance(fi, tuple), len(fi))
print(fi[0] == fi.max, fi[1] == fi.max_exp, fi[8] == fi.epsilon, fi[-1] == fi.rounds)
print(fi.max > 0.0, fi.min > 0.0, fi.epsilon < 1.0, fi.radix == 2)
print(fi.dig, fi.mant_dig, fi.max_exp, fi.min_exp, fi.max_10_exp, fi.min_10_exp)
print(fi[:3] == (fi.max, fi.max_exp, fi.max_10_exp), type(fi[:2]) is tuple)
print(1.0 + fi.epsilon > 1.0, fi.max * 2 == float('inf'))

# int_info: repr, tuple conversion, comparison with a plain tuple.
ii = sys.int_info
print(repr(ii))
print(tuple(ii), ii == (30, 4, 4300, 640), len(ii))
print(ii.bits_per_digit, ii.sizeof_digit, ii.default_max_str_digits, ii.str_digits_check_threshold)

# thread_info: repr and fields (name/lock/version; version is None here).
ti = sys.thread_info
print(repr(ti))
print(ti.name, ti.lock, ti.version, ti[0] == ti.name, ti[2] is None, len(ti))

# float_repr_style: 'short' on every engine in this cohort.
print(sys.float_repr_style)

# flags: the env-independent consumed fields (bool-ness of dev_mode and
# safe_path is part of the contract — the oracle prints False, not 0).
f = sys.flags
print(f.bytes_warning, f.utf8_mode, f.int_max_str_digits)
print(f.dev_mode, f.safe_path, f.debug, f.optimize, f.no_site, f.quiet)

# audit/addaudithook: no-op with no hooks; prefix-filtered hook round-trip.
print(sys.audit('pon-corpus-unheard', 1) is None)
events = []
def hook(event, args):
    if event.startswith('pon-corpus-'):
        events.append((event, args))
sys.addaudithook(hook)
print(sys.audit('pon-corpus-plain') is None)
sys.audit('pon-corpus-args', 1, 'x', (2, 3))
print(events)
try:
    sys.audit(123)
except TypeError as e:
    print('TypeError:', e)
try:
    sys.audit()
except TypeError as e:
    print('TypeError:', e)
def boom(event, args):
    if event == 'pon-corpus-boom':
        raise RuntimeError('audit boom')
sys.addaudithook(boom)
try:
    sys.audit('pon-corpus-boom')
except RuntimeError as e:
    print('RuntimeError:', e)

# getsizeof: shape only — sizes are implementation-defined by design.
print(isinstance(sys.getsizeof(0), int), sys.getsizeof(0) > 0)
print(isinstance(sys.getsizeof('abc'), int), sys.getsizeof([1, 2, 3]) > 0)
print(sys.getsizeof(object(), -1) != -1)
try:
    sys.getsizeof()
except TypeError as e:
    print('TypeError:', e)

# stdin: exists with the TextIO flag surface (newlines starts None).
print(sys.stdin is not None, hasattr(sys.stdin, 'newlines'), sys.stdin.newlines)
print(sys.stdin.readable(), sys.stdin.writable())

# path: a real mutable str list (contents are environment-specific).
print(isinstance(sys.path, list), len(sys.path) > 0)
print(all(isinstance(entry, str) for entry in sys.path))
marker = '/pon-corpus-sentinel'
sys.path.append(marker)
print(marker in sys.path, sys.path[-1] == marker)
sys.path.remove(marker)
print(marker not in sys.path)
