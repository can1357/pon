"""Source-compatible fallback for CPython's private _stat module on Darwin."""

ST_MODE = 0
ST_INO = 1
ST_DEV = 2
ST_NLINK = 3
ST_UID = 4
ST_GID = 5
ST_SIZE = 6
ST_ATIME = 7
ST_MTIME = 8
ST_CTIME = 9

S_IFDIR = 0o040000
S_IFCHR = 0o020000
S_IFBLK = 0o060000
S_IFREG = 0o100000
S_IFIFO = 0o010000
S_IFLNK = 0o120000
S_IFSOCK = 0o140000
S_IFWHT = 0o160000
S_IFDOOR = 0
S_IFPORT = 0

S_ISUID = 0o4000
S_ISGID = 0o2000
S_ENFMT = S_ISGID
S_ISVTX = 0o1000
S_IREAD = 0o0400
S_IWRITE = 0o0200
S_IEXEC = 0o0100
S_IRWXU = 0o0700
S_IRUSR = 0o0400
S_IWUSR = 0o0200
S_IXUSR = 0o0100
S_IRWXG = 0o0070
S_IRGRP = 0o0040
S_IWGRP = 0o0020
S_IXGRP = 0o0010
S_IRWXO = 0o0007
S_IROTH = 0o0004
S_IWOTH = 0o0002
S_IXOTH = 0o0001

UF_SETTABLE = 0x0000ffff
UF_NODUMP = 0x00000001
UF_IMMUTABLE = 0x00000002
UF_APPEND = 0x00000004
UF_OPAQUE = 0x00000008
UF_NOUNLINK = 0x00000010
UF_COMPRESSED = 0x00000020
UF_TRACKED = 0x00000040
UF_DATAVAULT = 0x00000080
UF_HIDDEN = 0x00008000
SF_SETTABLE = 0x3fff0000
SF_ARCHIVED = 0x00010000
SF_IMMUTABLE = 0x00020000
SF_APPEND = 0x00040000
SF_NOUNLINK = 0x00100000
SF_SNAPSHOT = 0x00200000
SF_FIRMLINK = 0x00800000
SF_DATALESS = 0x40000000
SF_SUPPORTED = 0x009f0000
SF_SYNTHETIC = 0xc0000000


def S_IMODE(mode):
    return mode & 0o7777


def S_IFMT(mode):
    return mode & 0o170000


def S_ISDIR(mode):
    return S_IFMT(mode) == S_IFDIR


def S_ISCHR(mode):
    return S_IFMT(mode) == S_IFCHR


def S_ISBLK(mode):
    return S_IFMT(mode) == S_IFBLK


def S_ISREG(mode):
    return S_IFMT(mode) == S_IFREG


def S_ISFIFO(mode):
    return S_IFMT(mode) == S_IFIFO


def S_ISLNK(mode):
    return S_IFMT(mode) == S_IFLNK


def S_ISSOCK(mode):
    return S_IFMT(mode) == S_IFSOCK


def S_ISDOOR(mode):
    return False


def S_ISPORT(mode):
    return False


def S_ISWHT(mode):
    return S_IFMT(mode) == S_IFWHT


def filemode(mode):
    perm = []
    for bit, char in ((S_IFLNK, 'l'), (S_IFSOCK, 's'), (S_IFREG, '-'),
                      (S_IFBLK, 'b'), (S_IFDIR, 'd'), (S_IFCHR, 'c'),
                      (S_IFIFO, 'p'), (S_IFWHT, 'w')):
        if S_IFMT(mode) == bit:
            perm.append(char)
            break
    else:
        perm.append('?')
    for bit, char in ((S_IRUSR, 'r'), (S_IWUSR, 'w')):
        perm.append(char if mode & bit else '-')
    if mode & S_ISUID:
        perm.append('s' if mode & S_IXUSR else 'S')
    else:
        perm.append('x' if mode & S_IXUSR else '-')
    for bit, char in ((S_IRGRP, 'r'), (S_IWGRP, 'w')):
        perm.append(char if mode & bit else '-')
    if mode & S_ISGID:
        perm.append('s' if mode & S_IXGRP else 'S')
    else:
        perm.append('x' if mode & S_IXGRP else '-')
    for bit, char in ((S_IROTH, 'r'), (S_IWOTH, 'w')):
        perm.append(char if mode & bit else '-')
    if mode & S_ISVTX:
        perm.append('t' if mode & S_IXOTH else 'T')
    else:
        perm.append('x' if mode & S_IXOTH else '-')
    return ''.join(perm)
