# Native math module surface: value families, exact int/bigint results,
# CPython 3.14 error legs, and the import names the vendored random/statistics
# chains bind at module top.
import math


def err(fn, *args, **kwargs):
    try:
        return repr(fn(*args, **kwargs))
    except Exception as exc:
        return type(exc).__name__ + ": " + str(exc)


# constants and classification
print(math.pi, math.e, math.tau, math.inf, -math.inf, math.nan)
print(math.isnan(math.nan), math.isinf(-math.inf), math.isfinite(1.5), math.isfinite(math.inf))

# roots, exponentials, logs
print(math.sqrt(2.0), math.sqrt(9), math.sqrt(0.0), math.cbrt(27.0), math.cbrt(-8.0))
print(math.exp(1.0), math.exp(-2.5), math.exp2(10), math.expm1(1e-05))
print(math.log(math.e), math.log(1024, 2), math.log2(1024), math.log10(1000))
print(math.log1p(1e-09), math.log(10**100), math.log2(2**2000), math.log10(10**500))
print(math.pow(2.0, 10), math.pow(-2.0, 3.0), math.pow(0.0, 0.0), math.pow(-1.0, math.inf))

# trig and hyperbolic
print(math.sin(1.0), math.cos(1.0), math.tan(1.0))
print(math.asin(0.5), math.acos(0.5), math.atan(2.0), math.atan2(1.0, -1.0))
print(math.sinh(1.0), math.cosh(1.0), math.tanh(1.0))
print(math.asinh(2.0), math.acosh(2.0), math.atanh(0.5))
print(math.degrees(math.pi), math.radians(180.0), math.degrees(1.0), math.radians(90))

# float structure
print(math.floor(3.7), math.ceil(3.2), math.trunc(-3.7), math.floor(-3.7), math.ceil(-3.2))
print(math.floor(10**30 / 7), math.ceil(1e308))
print(math.fabs(-5), math.copysign(3.0, -0.0), math.fmod(7.5, 2.0), math.fmod(-7.5, 2.0))
print(math.remainder(7.5, 2.0), math.remainder(5.0, 2.0), math.remainder(2.9, 2.0))
print(math.frexp(8.0), math.frexp(-0.5), math.frexp(0.0), math.modf(2.75), math.modf(-2.75))
print(math.ldexp(0.5, 4), math.ldexp(1.0, -1074), math.ldexp(-3.0, 0))
print(math.ulp(1.0), math.ulp(0.0), math.nextafter(1.0, 2.0), math.nextafter(1.0, 0.0), math.nextafter(0.0, -1.0))
print(math.nextafter(1.0, 2.0, steps=3), math.nextafter(1.0, math.inf, steps=0))
print(math.fma(2.0, 3.0, 1.0), math.fma(1e300, 1e-300, -1.0))

# exact integer family (bigint)
print(math.factorial(0), math.factorial(5), math.factorial(25))
print(math.isqrt(0), math.isqrt(24), math.isqrt(25), math.isqrt(10**40 - 1), math.isqrt(10**40))
print(math.gcd(), math.gcd(0, 0), math.gcd(12, 18, 24), math.gcd(10**30, 2**30), math.gcd(-12, 18))
print(math.lcm(), math.lcm(4, 6), math.lcm(0, 5), math.lcm(2**40, 3**20))
print(math.comb(10, 3), math.comb(0, 0), math.comb(5, 9), math.comb(100, 50))
print(math.perm(10, 3), math.perm(10), math.perm(5, 0), math.perm(5, 9), math.perm(30, 15))

# accurate summation and products
print(math.fsum([0.1] * 10), sum([0.1] * 10))
print(math.fsum([1e16, 1.0, 1e-16]), math.fsum([math.inf, math.inf]), math.fsum([]))
print(math.hypot(3.0, 4.0), math.hypot(), math.hypot(5), math.hypot(1e200, 1e200), math.hypot(3e-320, 4e-320))
print(math.dist((1.0, 2.0), (4.0, 6.0)), math.dist([0], [0]))
print(math.prod([1, 2, 3, 4]), math.prod([2] * 64), math.prod([1.5, 2.0], start=2), math.prod([], start=7))
print(math.prod([10**20, 10**20]), math.prod([0.5, 10**10]))
print(math.sumprod([1, 2, 3], [4, 5, 6]), math.sumprod([1.5, 2.5], [2.0, 4.0]), math.sumprod([10**20, 1], [10**20, 3]))
print(math.sumprod([], []), math.sumprod([1.5, 2], [4, 2.5]))

# isclose tolerance shapes
print(math.isclose(1.0, 1.0), math.isclose(1.0, 1.0 + 1e-10), math.isclose(1.0, 1.1))
print(math.isclose(1.0, 1.001, rel_tol=0.01), math.isclose(100.0, 100.4, rel_tol=0.0, abs_tol=0.5))
print(math.isclose(math.inf, math.inf), math.isclose(math.inf, 1e308), math.isclose(math.nan, math.nan))

# special functions: exact anchors printed, Lanczos-path values relational
print(math.gamma(5.0), math.gamma(1.0), math.gamma(23.0), math.lgamma(1.0), math.lgamma(2.0))
print(math.isclose(math.gamma(0.5), math.sqrt(math.pi), rel_tol=1e-14))
print(math.isclose(math.lgamma(10.0), math.log(math.factorial(9)), rel_tol=1e-14))
print(math.erf(0.0), math.erfc(0.0), round(math.erf(1.0), 12), round(math.erfc(2.0), 12))

# int/bool acceptance and protocol dispatch
print(math.sqrt(True), math.exp(False), math.floor(7), math.ceil(-7), math.trunc(9))


class Floaty:
    def __float__(self):
        return 2.25


class Indexy:
    def __index__(self):
        return 4


class Floory:
    def __floor__(self):
        return -99

    def __ceil__(self):
        return 100

    def __trunc__(self):
        return 11


print(math.sqrt(Floaty()), math.isqrt(Indexy()), math.gcd(Indexy(), 6))
print(math.floor(Floory()), math.ceil(Floory()), math.trunc(Floory()))

# error legs: 3.14 messages, typed and catchable
print(err(math.sqrt, -1))
print(err(math.log, 0))
print(err(math.log, -1.5))
print(err(math.log, 2, 1))
print(err(math.asin, 2))
print(err(math.acosh, 0.5))
print(err(math.atanh, 1.0))
print(err(math.cos, math.inf))
print(err(math.exp, 1000))
print(err(math.pow, 0.0, -2.0))
print(err(math.pow, 10.0, 400))
print(err(math.pow, -1.5, 2.5))
print(err(math.fmod, 1.0, 0.0))
print(err(math.remainder, 1.0, 0.0))
print(err(math.factorial, -1))
print(err(math.factorial, 5.5))
print(err(math.isqrt, -1))
print(err(math.comb, 5, -1))
print(err(math.perm, -5, 2))
print(err(math.gcd, "x"))
print(err(math.floor, math.nan))
print(err(math.ceil, math.inf))
print(err(math.fsum, [1e308, 1e308]))
print(err(math.fsum, [math.inf, -math.inf]))
print(err(math.isclose, 1.0, 2.0, rel_tol=-0.1))
print(err(math.dist, (1, 2), (1, 2, 3)))
print(err(math.sumprod, [1], [1, 2]))
print(err(math.ldexp, 1.0, 1.5))
print(err(math.ldexp, 1.0, 10**30))
print(err(math.nextafter, 1.0, 2.0, steps=-1))
print(err(math.trunc, "x"))
print(err(math.gamma, 0.0))
print(err(math.gamma, -2.0))
print(err(math.gamma, 1000.0))
print(err(math.lgamma, 0))
print(err(math.tan, "x"))

# names the vendored random.py / statistics.py bind at module top
from math import log as _log, exp as _exp, pi as _pi, e as _e, ceil as _ceil
from math import sqrt as _sqrt, acos as _acos, cos as _cos, sin as _sin
from math import tau as TWOPI, floor as _floor, isfinite as _isfinite
from math import lgamma as _lgamma, fabs as _fabs, log2 as _log2
from math import hypot, fabs, exp, erfc, tau, log, fsum, sumprod
from math import isfinite, isinf, cos, sin, tan, cosh, asin, atan, acos

print(_log(1.0), _ceil(0.5), _floor(0.5), _isfinite(0.0), _lgamma(1.0), _fabs(-2))
print(callable(hypot), callable(sumprod), callable(erfc), callable(cosh), isinf(TWOPI))
