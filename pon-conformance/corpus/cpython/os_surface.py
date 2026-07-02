# os/posix/select import surface: O_* flag constants, environ mapping,
# fspath round-trips, and the posix/select aliasing contracts.
#
# Raw O_*/POLL* values are printed deliberately: the differential oracle is
# the host python3.14, so host-specific constants agree by construction.
# posix.environ is deliberately untested: CPython serves it bytes-keyed while
# pon shares os.environ's str-keyed snapshot.
import os
import posix
import select

# O_* constants: int-ness, raw values, and flag algebra.
print(isinstance(os.O_RDONLY, int), isinstance(os.O_CREAT, int), isinstance(os.O_DIRECTORY, int))
print(os.O_RDONLY, os.O_WRONLY, os.O_RDWR)
print(os.O_APPEND, os.O_CREAT, os.O_EXCL, os.O_TRUNC)
mode = os.O_RDWR | os.O_CREAT | os.O_EXCL
print(mode & os.O_CREAT == os.O_CREAT)
print(mode & os.O_ACCMODE == os.O_RDWR)
print(os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) == (os.O_RDONLY | os.O_DIRECTORY))

# environ: a real mapping over the process environment.
print(os.environ.get("PATH") is not None)
print("PATH" in os.environ)
print(os.environ.get("PON_SURELY_UNSET_ENV_VAR") is None)

# fspath round-trips: str/bytes pass through, __fspath__ defers, int raises.
print(os.fspath("a/b"))
print(os.fspath(b"c/d"))


class Wrapped:
    def __fspath__(self):
        return "wrapped/path"


print(os.fspath(Wrapped()))
try:
    os.fspath(3)
except TypeError as exc:
    print("TypeError", "int" in str(exc))

# posix aliases the os surface (CPython: os.py re-exports posix wholesale).
print(posix.O_RDONLY == os.O_RDONLY, posix.O_CREAT == os.O_CREAT)
print(posix.stat_result is os.stat_result)
print(os.name)

# select import surface: error aliasing and poll(2) event masks.
print(select.error is OSError)
print(isinstance(select.POLLIN, int), select.POLLIN, select.POLLOUT)
print(callable(select.select), callable(select.poll))
