import os
import sys
import types
import typing
import platform

sys.maxunicode = 0x10ffff
platform.python_version = lambda: '3.14.6'

if not hasattr(os, 'makedirs'):
    def _probe_makedirs(name, mode=0o777, exist_ok=False):
        path = os.fspath(name)
        if not path:
            return None
        normalized = os.path.normpath(path)
        prefix = ''
        rest = normalized
        if normalized.startswith(os.sep):
            prefix = os.sep
            rest = normalized[1:]
        current = prefix
        for part in rest.split(os.sep):
            if not part:
                continue
            current = os.path.join(current, part) if current else part
            try:
                os.mkdir(current, mode)
            except FileExistsError:
                if not exist_ok and current == normalized:
                    raise
        return None
    os.makedirs = _probe_makedirs

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

sys.path.insert(0, 'vendored-meson/meson')
import mesonbuild.mlog as mlog
mlog._logger.colorize_console = lambda: False
mlog.colorize_console = mlog._logger.colorize_console
from mesonbuild import mesonmain
sys.argv = ['vendored-meson/meson/meson.py', 'setup', '/tmp/pon_meson_btest', '--reconfigure']
sys.exit(mesonmain.main())
