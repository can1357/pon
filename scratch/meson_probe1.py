import sys
sys.path.insert(0, '/tmp/numpy_src/numpy-2.5.0/vendored-meson/meson')
print('importing mesonbuild')
import mesonbuild
print('mesonbuild ok', mesonbuild)
print('importing mesonmain')
from mesonbuild import mesonmain
print('mesonmain ok')
