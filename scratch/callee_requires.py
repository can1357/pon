_pon_backend_module = __import__('mesonpy', fromlist=['_pon_backend'])
_pon_backend = _pon_backend_module
_pon_hook = getattr(_pon_backend, 'get_requires_for_build_wheel', None)
_pon_requirements = []
import traceback as _pon_tb
if _pon_hook is not None:
    try:
        print('HOOK', type(_pon_hook), repr(_pon_hook))
        _pon_requirements = _pon_hook(None)
        print('REQUIRES', repr(_pon_requirements))
    except BaseException:
        _pon_tb.print_exc()
        raise
for _pon_requirement in _pon_requirements:
    print('REQ', str(_pon_requirement))
