# fcntl.flock constants, file-object fileno coercion, conflicts, and errors.
import fcntl
import os

print("LOCK_SH", fcntl.LOCK_SH)
print("LOCK_EX", fcntl.LOCK_EX)
print("LOCK_NB", fcntl.LOCK_NB)
print("LOCK_UN", fcntl.LOCK_UN)

path = "/tmp/pon_fcntl_flock_basic_%d" % os.getpid()

with open(path, "w+") as first:
    first.write("lock me")
    first.flush()
    second_fd = os.open(path, os.O_RDWR)
    try:
        fcntl.flock(first, fcntl.LOCK_EX | fcntl.LOCK_NB)
        print("object lock", True)
        try:
            fcntl.flock(second_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            print("conflict", type(exc).__name__, exc.errno in (11, 35))
        fcntl.flock(first.fileno(), fcntl.LOCK_UN)
        print("raw unlock", True)
        fcntl.flock(second_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        print("second lock", True)
        fcntl.flock(second_fd, fcntl.LOCK_UN)
    finally:
        os.close(second_fd)

try:
    os.remove(path)
except OSError:
    pass

try:
    fcntl.flock("not-a-fd", fcntl.LOCK_EX)
except TypeError as exc:
    print("bad fd", type(exc).__name__, str(exc))
