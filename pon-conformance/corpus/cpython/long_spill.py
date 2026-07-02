# Numeric tower exactness canaries for int spill paths.

near_pos = (1 << 63) - 1
spill_pos = 1 << 63
spill_neg = -(1 << 63) - 1
huge_a = (1 << 130) + (1 << 65) + 12345
huge_b = (1 << 96) - 98765
mask = (1 << 72) - 1

print("bounds", near_pos, spill_pos, spill_neg)
print("add-sub", (spill_pos + 17) - spill_pos, spill_neg + spill_pos)
print("mul", ((1 << 64) + 3) * ((1 << 64) - 5))
print("neg-abs", -spill_neg, abs(spill_neg), abs(-7))

print("bitwise", huge_a & mask, huge_a | mask, huge_a ^ mask)
print("invert", ~spill_pos, ~spill_neg)
print("shifts", spill_pos << 5, huge_a >> 64, (-huge_a) >> 64)

print("floor", huge_a // 97, huge_a % 97)
print("floor-neg", (-huge_a) // 97, (-huge_a) % 97)
print("floor-divisor-neg", huge_a // -97, huge_a % -97)
print("divmod", divmod(huge_a, huge_b))
print("divmod-neg", divmod(-huge_a, huge_b))

print("pow", 2 ** 100, (3 ** 80) % 1009, (-3) ** 17)
print("compare", near_pos < spill_pos, spill_neg < -spill_pos, huge_a > huge_b, huge_a == huge_b)
print(hash(2**61-1), hash(2**61), hash(-1), hash(2**64))
