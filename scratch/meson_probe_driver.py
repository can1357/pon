import runpy
import sys
import types

m = types.ModuleType('_scproxy')
m._get_proxy_settings = lambda: {}
m._get_proxies = lambda: {}
sys.modules['_scproxy'] = m

sys.argv = ['vendored-meson/meson/meson.py', '--version']
runpy.run_path('vendored-meson/meson/meson.py', run_name='__main__')
