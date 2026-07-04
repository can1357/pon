_pon_backend_module = __import__('mesonpy', fromlist=['_pon_backend'])
_pon_backend = _pon_backend_module
import traceback as _pon_tb
try:
    print('BACKEND', type(_pon_backend), repr(_pon_backend))
    print('BUILD_WHEEL', type(_pon_backend.build_wheel), repr(_pon_backend.build_wheel))
    _pon_backend.build_wheel('/work/pon/tmp/callee_wheelhouse')
except BaseException:
    _pon_tb.print_exc()
    raise
