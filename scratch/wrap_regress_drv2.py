import sys
sys.path.insert(0, '/tmp/numpy_src/numpy-2.5.0/vendored-meson/meson')
from mesonbuild import mesonmain
print('MESONMAIN_OK')
from mesonbuild.wrap import wrap
print('WRAP_OK')
