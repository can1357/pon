import os

base = '/tmp/pon_os_surface_probe_' + str(os.getpid())
nested = base + '/a/b/c'
mode_leaf = base + '/mode'
file_path = base + '/file'
file2_path = base + '/file2'


def quiet_unlink(path):
    try:
        os.unlink(path)
    except OSError:
        pass


def quiet_rmdir(path):
    try:
        os.rmdir(path)
    except OSError:
        pass


def clean():
    quiet_unlink(file_path)
    quiet_unlink(file2_path)
    quiet_rmdir(mode_leaf)
    quiet_rmdir(nested)
    quiet_rmdir(base + '/a/b')
    quiet_rmdir(base + '/a')
    quiet_rmdir(base)


def touch(path):
    fd = os.open(path, os.O_CREAT | os.O_WRONLY | os.O_TRUNC, 0o600)
    os.close(fd)


clean()
try:
    os.mkdir(base)

    os.makedirs(nested)
    print('nested', (os.stat(nested).st_mode & 0o170000) == 0o040000)

    try:
        os.makedirs(nested, exist_ok=True)
    except Exception as exc:
        print('repeat_true', type(exc).__name__)
    else:
        print('repeat_true', 'OK')

    try:
        os.makedirs(nested, exist_ok=False)
    except Exception as exc:
        print('repeat_false', type(exc).__name__)
    else:
        print('repeat_false', 'OK')

    touch(file_path)
    try:
        os.makedirs(file_path, exist_ok=True)
    except Exception as exc:
        print('file_exist_ok_true', type(exc).__name__)
    else:
        print('file_exist_ok_true', 'OK')

    old_umask = os.umask(0o027)
    try:
        os.makedirs(mode_leaf, mode=0o765)
    finally:
        os.umask(old_umask)
    print('mode', oct(os.stat(mode_leaf).st_mode & 0o777))

    touch(file2_path)
    s1 = os.stat(mode_leaf)
    s2 = os.stat(mode_leaf)
    f1 = os.stat(file_path)
    f2 = os.stat(file2_path)
    print('ino_same_file', s1.st_ino == s2.st_ino)
    print('dev_same_tmpdir', f1.st_dev == f2.st_dev)
    print('nlink_ge_1', s1.st_nlink >= 1)
    print('time_types', type(s1.st_atime).__name__, type(s1.st_ctime).__name__)
finally:
    clean()
