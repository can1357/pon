"""Small source-compatible cmath fallback."""

from math import atan2 as _atan2, cos as _cos, cosh as _cosh, e, exp as _exp
from math import hypot as _hypot, inf, isfinite as _isfinite, isinf as _isinf
from math import isnan as _isnan, log as _real_log, pi, sin as _sin
from math import sinh as _sinh, sqrt as _real_sqrt, tau

nan = inf - inf
infj = complex(0.0, inf)
nanj = complex(0.0, nan)


def _as_complex(z):
    return complex(z)


def phase(z, _as_complex=_as_complex, _atan2=_atan2):
    z = _as_complex(z)
    return _atan2(z.imag, z.real)


def polar(z, _as_complex=_as_complex, _hypot=_hypot, _atan2=_atan2):
    z = _as_complex(z)
    return (_hypot(z.real, z.imag), _atan2(z.imag, z.real))


def rect(r, phi, _cos=_cos, _sin=_sin):
    return complex(r * _cos(phi), r * _sin(phi))


def isfinite(z, _as_complex=_as_complex, _isfinite=_isfinite):
    z = _as_complex(z)
    return _isfinite(z.real) and _isfinite(z.imag)


def isinf(z, _as_complex=_as_complex, _isinf=_isinf):
    z = _as_complex(z)
    return _isinf(z.real) or _isinf(z.imag)


def isnan(z, _as_complex=_as_complex, _isnan=_isnan):
    z = _as_complex(z)
    return _isnan(z.real) or _isnan(z.imag)


def isclose(a, b, *, rel_tol=1e-09, abs_tol=0.0, _as_complex=_as_complex,
            _isinf=_isinf, _isnan=_isnan):
    if rel_tol < 0.0 or abs_tol < 0.0:
        raise ValueError('tolerances must be non-negative')
    a = _as_complex(a)
    b = _as_complex(b)
    if a == b:
        return True
    if (_isinf(a.real) or _isinf(a.imag) or _isinf(b.real) or _isinf(b.imag) or
            _isnan(a.real) or _isnan(a.imag) or _isnan(b.real) or _isnan(b.imag)):
        return False
    diff = abs(b - a)
    return diff <= max(rel_tol * max(abs(a), abs(b)), abs_tol)


def exp(z, _as_complex=_as_complex, _exp=_exp, _cos=_cos, _sin=_sin):
    z = _as_complex(z)
    scale = _exp(z.real)
    return complex(scale * _cos(z.imag), scale * _sin(z.imag))


def log(z, base=None, _as_complex=_as_complex, _hypot=_hypot, _atan2=_atan2,
        _real_log=_real_log):
    z = _as_complex(z)
    result = complex(_real_log(_hypot(z.real, z.imag)), _atan2(z.imag, z.real))
    if base is not None:
        base = log(base)
        result = result / base
    return result


def log10(z, log=log, _real_log=_real_log):
    return log(z) / _real_log(10.0)


def sqrt(z, _as_complex=_as_complex, _hypot=_hypot, _real_sqrt=_real_sqrt):
    z = _as_complex(z)
    if z.real == 0.0 and z.imag == 0.0:
        return complex(0.0, z.imag)
    r = _hypot(z.real, z.imag)
    if z.real >= 0.0:
        real = _real_sqrt((r + z.real) / 2.0)
        imag = z.imag / (2.0 * real)
    else:
        imag = _real_sqrt((r - z.real) / 2.0)
        if z.imag < 0.0:
            imag = -imag
        real = z.imag / (2.0 * imag) if imag != 0.0 else 0.0
    return complex(real, imag)


def sin(z, _as_complex=_as_complex, _sin=_sin, _cos=_cos, _sinh=_sinh, _cosh=_cosh):
    z = _as_complex(z)
    return complex(_sin(z.real) * _cosh(z.imag), _cos(z.real) * _sinh(z.imag))


def cos(z, _as_complex=_as_complex, _sin=_sin, _cos=_cos, _sinh=_sinh, _cosh=_cosh):
    z = _as_complex(z)
    return complex(_cos(z.real) * _cosh(z.imag), -_sin(z.real) * _sinh(z.imag))


def tan(z, sin=sin, cos=cos):
    return sin(z) / cos(z)


def sinh(z, _as_complex=_as_complex, _sin=_sin, _cos=_cos, _sinh=_sinh, _cosh=_cosh):
    z = _as_complex(z)
    return complex(_sinh(z.real) * _cos(z.imag), _cosh(z.real) * _sin(z.imag))


def cosh(z, _as_complex=_as_complex, _sin=_sin, _cos=_cos, _sinh=_sinh, _cosh=_cosh):
    z = _as_complex(z)
    return complex(_cosh(z.real) * _cos(z.imag), _sinh(z.real) * _sin(z.imag))


def tanh(z, sinh=sinh, cosh=cosh):
    return sinh(z) / cosh(z)


def asin(z, _as_complex=_as_complex, sqrt=sqrt, log=log):
    z = _as_complex(z)
    return -1j * log(1j * z + sqrt(1.0 - z * z))


def acos(z, asin=asin, pi=pi):
    return pi / 2.0 - asin(z)


def atan(z, _as_complex=_as_complex, log=log):
    z = _as_complex(z)
    return (log(1.0 + 1j * z) - log(1.0 - 1j * z)) / (2j)


def asinh(z, _as_complex=_as_complex, sqrt=sqrt, log=log):
    z = _as_complex(z)
    return log(z + sqrt(z * z + 1.0))


def acosh(z, _as_complex=_as_complex, sqrt=sqrt, log=log):
    z = _as_complex(z)
    return log(z + sqrt(z + 1.0) * sqrt(z - 1.0))


def atanh(z, _as_complex=_as_complex, log=log):
    z = _as_complex(z)
    return (log(1.0 + z) - log(1.0 - z)) / 2.0


del _atan2, _cos, _cosh, _exp, _hypot, _isfinite, _isinf, _isnan
del _real_log, _real_sqrt, _sin, _sinh, _as_complex
