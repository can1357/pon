"""Source-compatible fallback for CPython's private _operator module."""

from operator import abs, add, and_, attrgetter, call, concat, contains, countOf
from operator import delitem, eq, floordiv, ge, getitem, gt, iadd, iand
from operator import iconcat, ifloordiv, ilshift, imatmul, imod, imul
from operator import index, indexOf, inv, invert, ior, ipow, irshift
from operator import is_, is_none, is_not, is_not_none, isub, itemgetter
from operator import itruediv, ixor, le, length_hint, lshift, lt, matmul
from operator import methodcaller, mod, mul, ne, neg, not_, or_, pos, pow
from operator import rshift, setitem, sub, truediv, truth, xor


def _compare_digest(a, b):
    """Return a == b using content-independent work for equal-type inputs."""
    if isinstance(a, str) and isinstance(b, str):
        left = []
        right = []
        for ch in a:
            code = ord(ch)
            if code > 127:
                raise TypeError("comparing strings with non-ASCII characters is not supported")
            left.append(code)
        for ch in b:
            code = ord(ch)
            if code > 127:
                raise TypeError("comparing strings with non-ASCII characters is not supported")
            right.append(code)
    elif isinstance(a, (bytes, bytearray)) and isinstance(b, (bytes, bytearray)):
        left = bytes(a)
        right = bytes(b)
    else:
        raise TypeError("unsupported operand types(s) or combination of types")

    result = len(left) ^ len(right)
    limit = max(len(left), len(right))
    for i in range(limit):
        x = left[i] if i < len(left) else 0
        y = right[i] if i < len(right) else 0
        result |= x ^ y
    return result == 0
