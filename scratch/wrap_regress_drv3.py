import sys
sys.path.insert(0, '/tmp/numpy_src/numpy-2.5.0/vendored-meson/meson')
from mesonbuild import mesonmain
print('mesonmain ok', flush=True)
import mesonbuild.msetup
print('msetup ok', flush=True)
import mesonbuild.mconf
print('mconf ok', flush=True)
import mesonbuild.mintro
print('mintro ok', flush=True)
from mesonbuild.wrap import wrap
print('wrap ok', flush=True)
