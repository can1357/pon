import sys
sys.path.insert(0, '/tmp/numpy_src/numpy-2.5.0/vendored-meson/meson')
print('importing mesonbuild.wrap')
from mesonbuild import wrap
print('wrap pkg ok', wrap.WrapMode)
print('importing wrap.wrap')
from mesonbuild.wrap import wrap as wrapwrap
print('wrap.wrap ok')
