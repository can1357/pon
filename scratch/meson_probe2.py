import sys
from pathlib import Path
print('argv0:', sys.argv[0])
meson_exe = Path(sys.argv[0]).resolve()
print('meson_exe:', meson_exe)
print('parent/mesonbuild is_dir:', (meson_exe.parent / 'mesonbuild').is_dir())
if (meson_exe.parent / 'mesonbuild').is_dir():
    sys.path.insert(0, str(meson_exe.parent))
print('sys.path[0]:', sys.path[0])
from mesonbuild import mesonmain
print('mesonmain ok')
