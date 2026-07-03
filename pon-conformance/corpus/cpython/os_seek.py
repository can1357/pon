# os seek surface: the SEEK_* whence constants, fd-level lseek round-trips,
# file-object seek(offset, whence) parity (the zipfile shape: negative
# offsets from SEEK_END scanning for the end-of-central-directory record),
# and os.devnull.  Raw constant values are printed deliberately: the
# differential oracle is the host python3.14, so host-specific values
# (SEEK_HOLE/SEEK_DATA swap between darwin and linux) agree by
# construction.  devnull reads are deterministic: always empty.

import os

# Whence constants: the portable trio is fixed by POSIX; HOLE/DATA are host.
print(os.SEEK_SET, os.SEEK_CUR, os.SEEK_END)
print(isinstance(os.SEEK_HOLE, int), isinstance(os.SEEK_DATA, int))
print(os.SEEK_SET == 0 and os.SEEK_CUR == 1 and os.SEEK_END == 2)

# fd-level lseek round-trip over a scratch file.
path = 'target/pon_os_seek_corpus.bin'
fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_TRUNC)
print(os.write(fd, b'0123456789'))
print(os.lseek(fd, 0, os.SEEK_SET), os.read(fd, 3))
print(os.lseek(fd, 2, os.SEEK_CUR), os.read(fd, 2))
print(os.lseek(fd, -4, os.SEEK_END), os.read(fd, 4))
print(os.lseek(fd, 0, os.SEEK_END))
try:
    os.lseek(fd, -100, os.SEEK_SET)
except OSError as e:
    print('OSError:', e.errno == 22)
os.close(fd)

# File-object whence seeks, binary mode (the zipfile _EndRecData shape).
with open(path, 'rb') as f:
    f.seek(-4, os.SEEK_END)
    print(f.read())
    f.seek(0, os.SEEK_SET)
    print(f.read(1))
    f.seek(3, os.SEEK_CUR)
    print(f.read(2), f.tell())
os.unlink(path)

# devnull: a non-regular file that exists and always reads empty.
print(os.devnull)
print(os.path.exists(os.devnull), os.path.isfile(os.devnull))
with open(os.devnull, 'rb') as f:
    print(f.read() == b'')
