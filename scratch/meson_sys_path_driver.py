import os
import sys

meson_exe = os.path.realpath('/tmp/numpy_src/numpy-2.5.0/vendored-meson/meson/meson.py')
meson_dir = os.path.dirname(meson_exe)
if os.path.isdir(os.path.join(meson_dir, 'mesonbuild')):
    sys.path.insert(0, meson_dir)

import mesonbuild
print('vendored', '/vendored-meson/meson/mesonbuild/' in mesonbuild.__file__)
from mesonbuild import coredata
print('version', coredata.version)
