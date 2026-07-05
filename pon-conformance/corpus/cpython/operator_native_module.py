# _operator native module: operator functions are builtins, so a class
# attribute like glob._StringGlobber's `concat_path = operator.add` must NOT
# bind the instance (pathlib.Path.glob drives meson's framework probes
# through that shape).
import operator


class C:
    concat = operator.add


print(C().concat(1, 2))
print(operator.add('a', 'b'), operator.sub(5, 2), operator.mul(3, 4))
print(operator.truediv(7, 2), operator.floordiv(7, 2), operator.mod(7, 3))
print(operator.lt(1, 2), operator.le(2, 2), operator.eq('a', 'a'))
print(operator.ne(1, 2), operator.ge(1, 2), operator.gt(2, 1))
print(operator.not_(0), operator.truth([1]))
print(operator.is_(None, None), operator.is_not(1, None))
print(operator.and_(6, 3), operator.or_(4, 1), operator.xor(5, 3))
print(operator.lshift(1, 4), operator.rshift(16, 2))
print(operator.neg(5), operator.pos(-5), operator.invert(0), operator.inv(1))
print(operator.abs(-3), operator.pow(2, 5), operator.index(True))
print(operator.getitem([1, 2, 3], 1), operator.contains('abc', 'b'))
d = {}
operator.setitem(d, 'k', 5)
print(d)
operator.delitem(d, 'k')
print(d)
lst = [1]
print(operator.iadd(lst, [2]), lst)
print(operator.__add__(1, 2), operator.__lt__(1, 2), operator.__not__(1))
try:
    operator.add(1)
except TypeError:
    print('TypeError')
try:
    operator.truediv(1, 0)
except ZeroDivisionError as exc:
    print('ZeroDivisionError', exc)
