import sys
import types

m = types.ModuleType('_scproxy')
m._get_proxy_settings = lambda: {}
m._get_proxies = lambda: {}
sys.modules['_scproxy'] = m

path = 'vendored-meson/meson/meson.py'
sys.argv = [path, '--version']
source = open(path, 'r', encoding='utf-8').read()
exec(compile(source, path, 'exec'), {'__name__': '__main__', '__file__': path})
