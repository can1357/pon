import traceback as _pon_tb
print('SCRIPT start')
_pon_backend_module = __import__('mesonpy', fromlist=['_pon_backend'])
print('SCRIPT imported mesonpy')
_pon_backend = _pon_backend_module
try:
    print('SCRIPT calling build_wheel')
    _pon_backend.build_wheel('/tmp/pon_debug_wheel')
    print('SCRIPT build_wheel returned')
except BaseException:
    print('SCRIPT exception')
    _pon_tb.print_exc()
    raise
