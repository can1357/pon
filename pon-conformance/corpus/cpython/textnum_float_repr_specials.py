# Derived from CPython v3.14.0 Lib/test/test_float.py topics (PSF license).

pos_inf = float("inf")
neg_inf = float("-inf")
pos_nan = float("nan")
neg_nan = float("-nan")
neg_zero = -0.0
pos_zero = 0.0

print(repr(pos_inf))
print(repr(neg_inf))
print(str(pos_inf))
print(str(neg_inf))

print(repr(pos_nan))
print(repr(neg_nan))
print(str(pos_nan))
print(str(neg_nan))

print(repr(neg_zero))
print(str(neg_zero))
print(repr(pos_zero))
print(str(pos_zero))

huge = 1e300 * 1e300
neg_huge = -1e300 * 1e300
made_nan = huge * 0.0
neg_made_nan = neg_huge * 0.0

print(repr(huge))
print(repr(neg_huge))
print(str(huge))
print(str(neg_huge))
print(repr(made_nan))
print(repr(neg_made_nan))
print(str(made_nan))
print(str(neg_made_nan))
