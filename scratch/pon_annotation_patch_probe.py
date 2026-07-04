_pon_sys = __import__('sys')
_pon_re = __import__('re')
if _pon_sys.implementation.name == 'pon':
    try:
        _pon_builtins = __import__('builtins')
        _pon_builtins.pathlib = __import__('pathlib')
    except Exception:
        pass
    try:
        import packaging.version as _pon_packaging_version
        _pon_packaging_version.VERSION_PATTERN = _pon_packaging_version._VERSION_PATTERN_OLD
        _pon_packaging_version.Version._regex = _pon_re.compile(r'\s*' + _pon_packaging_version.VERSION_PATTERN + r'\s*', _pon_re.VERBOSE | _pon_re.IGNORECASE)
    except Exception:
        pass
    try:
        import annotationlib as _pon_annotationlib
        _pon_real_call_annotate = _pon_annotationlib.call_annotate_function
        def _pon_call_annotate_function(annotate, format, *, owner=None, _is_evaluate=False):
            try:
                return _pon_real_call_annotate(annotate, format, owner=owner, _is_evaluate=_is_evaluate)
            except TypeError as _pon_exc:
                if 'function() takes no keyword arguments' not in str(_pon_exc):
                    raise
                return annotate(_pon_annotationlib.Format.VALUE)
        _pon_annotationlib.call_annotate_function = _pon_call_annotate_function
    except Exception:
        pass
from __future__ import annotations
import dataclasses

@dataclasses.dataclass
class License:
    file: pathlib.Path | None

print(License)
