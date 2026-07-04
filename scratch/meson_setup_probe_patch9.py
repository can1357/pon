import os
import sys
import types
import typing
import platform
import pathlib

sys.maxunicode = 0x10ffff
platform.python_version = lambda: '3.14.6'

class _ProbePathParents:
    def __init__(self, path):
        self._items = []
        p = path.parent
        while True:
            self._items.append(p)
            if p == p.parent:
                break
            p = p.parent
    def __len__(self):
        return len(self._items)
    def __getitem__(self, index):
        return self._items[index]
    def __iter__(self):
        return iter(self._items)

pathlib._PathParents = _ProbePathParents
pathlib.PurePath.parents = property(lambda self: _ProbePathParents(self))

_orig_stat = os.stat
class _StatProxy:
    def __init__(self, value):
        self._value = value
        self.st_ino = getattr(value, 'st_ino', 0)
        self.st_dev = getattr(value, 'st_dev', 0)
    def __getattr__(self, name):
        return getattr(self._value, name)
    def __getitem__(self, index):
        return self._value[index]
    def __len__(self):
        return len(self._value)
    def __iter__(self):
        return iter(self._value)

def _probe_stat(*args, **kwargs):
    return _StatProxy(_orig_stat(*args, **kwargs))
os.stat = _probe_stat

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
                pass
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
