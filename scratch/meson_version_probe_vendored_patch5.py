import sys
import types
import typing
import platform

sys.maxunicode = 0x10ffff
platform.python_version = lambda: '3.14.6'

m = types.ModuleType('_scproxy')
m._get_proxy_settings = lambda: {}
m._get_proxies = lambda: {}
sys.modules['_scproxy'] = m

lsprof = types.ModuleType('_lsprof')
class Profiler:
    def __init__(self, *args, **kwargs):
        pass
    def enable(self, *args, **kwargs):
        pass
    def disable(self, *args, **kwargs):
        pass
    def getstats(self):
        return []
lsprof.Profiler = Profiler
sys.modules['_lsprof'] = lsprof

def _probe_new_type(name, tp):
    def new_type(value):
        return value
    new_type.__name__ = name
    new_type.__qualname__ = name
    new_type.__supertype__ = tp
    return new_type

typing.NewType = _probe_new_type

# sys.path mutation is ignored by pon imports today; the shell env for this probe
# prepends vendored-meson/meson to PON_IMPORT_PATH/PONPATH.
import mesonbuild.compilers.compilers as compilers
compilers.all_languages = set(compilers.all_languages)

from mesonbuild import mesonmain
sys.argv = ['vendored-meson/meson/meson.py', '--version']
sys.exit(mesonmain.main())
