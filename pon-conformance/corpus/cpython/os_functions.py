# os fd/path syscall surface: open/write/read/close round-trip, unlink,
# lstat, errno -> OSError subclass mapping, the os.path aliasing contract,
# capability frozensets, and the os.PathLike protocol.
#
# The scratch file is workspace-relative (the runner's CWD has target/, the
# file_io.py convention) so pon and the host python3.14 see identical paths.
# Capability-set membership is only asserted for functions CPython ALSO
# excludes (os.read/os.close take fds, never dir_fd/follow_symlinks): the
# host sets are populated while pon's are deliberately empty, so pon-only
# emptiness (os.stat not in os.supports_follow_symlinks) is not printable
# under a differential oracle.
import os
import sys

# os.path via plain `import os` (the importer's deferred alias hook).
print(os.path.lexists("."))
print(os.path.lexists("target/pon_os_functions_missing"))

import os.path

# os.path via explicit `import os.path`: one module object under both names.
print(os.path is sys.modules["os.path"])
print(os.path.lexists("."))

# open/write/read/close round-trip over the raw fd surface.
path = "target/pon_os_functions_corpus.bin"
fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
print(isinstance(fd, int), fd > 2)
print(os.write(fd, b"pon:os:roundtrip"))
print(os.close(fd))
fd = os.open(path, os.O_RDONLY)
print(os.read(fd, 4))
print(os.read(fd, 64))
print(os.read(fd, 8))
os.close(fd)

# lstat serves live paths; unlink removes them; lexists flips.
print(os.lstat(path).st_size)
print(os.path.lexists(path))
os.unlink(path)
print(os.path.lexists(path))
try:
    os.lstat(path)
except FileNotFoundError as exc:
    print("FileNotFoundError", "[Errno 2]" in str(exc), isinstance(exc, OSError))

# errno -> subclass mapping on the open path, and O_EXCL creation.
try:
    os.open("target/pon_os_functions_missing_dir/x", os.O_RDONLY)
except FileNotFoundError:
    print("open FileNotFoundError")
fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
os.close(fd)
try:
    os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL)
except FileExistsError:
    print("open FileExistsError")
os.unlink(path)

# write() rejects str before touching the fd.
try:
    os.write(0, "text")
except TypeError:
    print("write TypeError")

# Capability sets: real (plain, CPython-shaped) sets; fd-taking functions
# are members of none of them on any platform.
print(isinstance(os.supports_dir_fd, set), isinstance(os.supports_fd, set))
print(isinstance(os.supports_follow_symlinks, set))
print(os.read in os.supports_dir_fd, os.close in os.supports_follow_symlinks)

# os.PathLike: structural isinstance via __fspath__, subclassing, registry.
print(isinstance("x", os.PathLike), isinstance(3, os.PathLike))


class FsPath:
    def __fspath__(self):
        return "fs/path"


print(isinstance(FsPath(), os.PathLike), issubclass(FsPath, os.PathLike))
print(os.fspath(FsPath()))


class Sub(os.PathLike):
    def __fspath__(self):
        return "sub/path"


print(isinstance(Sub(), os.PathLike))
print(os.fspath(Sub()))


class Registered:
    pass


print(os.PathLike.register(Registered) is Registered)
print(issubclass(Registered, os.PathLike), isinstance(Registered(), os.PathLike))

alias = os.PathLike[str]
print(alias.__origin__ is os.PathLike, alias.__args__ == (str,))
