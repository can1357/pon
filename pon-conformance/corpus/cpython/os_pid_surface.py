# os.getpid surface: int-ness and within-run stability of the process id,
# plus the os.py fs-codec pair (fsencode/fsdecode) served by the same native
# seed.  The raw pid is deliberately never printed: it differs between the
# pon process and the host-python oracle process by construction, so only
# derived predicates (type, positivity, equality-within-run) are
# differential-stable.
import os
import posix

pid = os.getpid()
print(type(pid) is int, isinstance(pid, bool))
print(pid > 0)
print(pid == os.getpid())

# One process, two module names: posix re-exports the same syscall surface,
# and the pid is process-global no matter which module answers.
print(posix.getpid() == pid)

# fs-codec pair: pure UTF-8 transforms, so raw values print identically
# under pon and the host oracle.
print(os.fsencode("a/\u00e6"))
print(os.fsdecode(b"a/\xc3\xa6"))
print(os.fsencode(b"raw-bytes"), os.fsdecode("already-str"))


class Wrapped:
    def __fspath__(self):
        return "wrapped/\u00e6"


print(os.fsencode(Wrapped()), os.fsdecode(Wrapped()))
try:
    os.fsencode(3)
except TypeError as exc:
    print("TypeError", "int" in str(exc))
