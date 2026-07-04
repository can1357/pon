import os
import sys

print('script-dir-first', os.path.basename(sys.path[0]) == 'tmp')


def ensure_dir(path):
    try:
        os.mkdir(path)
    except Exception:
        pass


def write_package(root, name, body):
    ensure_dir(root)
    package_dir = os.path.join(root, name)
    ensure_dir(package_dir)
    with open(os.path.join(package_dir, '__init__.py'), 'w') as handle:
        handle.write(body)


repro_a_root = '/tmp/sp_pkg'
write_package(repro_a_root, 'mylib', 'X = 1\n')
sys.path.insert(0, repro_a_root)
import mylib
print('repro-a', mylib.X)

ponpath_root = '/tmp/pon_sys_path_shadow_ponpath'
sys_path_root = '/tmp/pon_sys_path_shadow_inserted'
write_package(ponpath_root, 'shadowprobe', "ORIGIN = 'ponpath'\n")
write_package(sys_path_root, 'shadowprobe', "ORIGIN = 'sys-path'\n")
os.environ['PONPATH'] = ponpath_root
os.environ['PON_IMPORT_PATH'] = ponpath_root
sys.path.append(ponpath_root)
sys.path.insert(0, sys_path_root)
import shadowprobe
print('shadow', shadowprobe.ORIGIN)
