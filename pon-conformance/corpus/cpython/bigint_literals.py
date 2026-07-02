# Integer literals wider than i64 in decimal, hex, octal, and binary forms.

# Decimal literals beyond 2**63.
big_dec = 123456789012345678901234567890
print(big_dec)
print(big_dec + 1)
print(-big_dec)
print(big_dec * 2)

# Fits u64 but not i64.
print(18446744073709551615)
print(18446744073709551615 == (1 << 64) - 1)

# i64 boundary neighbors: fast path vs bigint path.
print(9223372036854775807)
print(9223372036854775808)
print(-9223372036854775808)
print(-9223372036854775809)

# Radix forms: prefixes in both cases, underscores, underscore after prefix.
print(0xCAFEBABE_DEADBEEF_0123_4567_89AB)
print(0XFFFFFFFFFFFFFFFFFF)
print(0x_ffff_ffff_ffff_ffff_f)
print(0o7777777777777777777777777777)
print(0O123_4567_0123_4567_0123_4567)
print(0b1111111111111111111111111111111111111111111111111111111111111111111111)
print(0B1010_1010_1010_1010_1010_1010_1010_1010_1010_1010_1010_1010_1010_1010_1010_1010_1)

# Same value spelled in different bases.
print(0x100000000000000000000 == 16 ** 20)
print((0b1 << 70) == 0x400000000000000000)

# Comparisons against computed bigints.
computed = 1 << 100
literal = 1267650600228229401496703205376
print(literal == computed)
print(literal < computed + 1)
print(literal > computed - 1)
print(literal - computed)
print(2 ** 200 == 1606938044258990275541962092341162602522202993782792835301376)

# Dict keys.
table = {
    340282366920938463463374607431768211455: "u128max",
    0x1_0000_0000_0000_0000: "two-to-64",
    99999999999999999999: "twenty-nines",
    -170141183460469231731687303715884105728: "i128min",
}
print(table[340282366920938463463374607431768211455])
print(table[2 ** 64])
print(table[10 ** 20 - 1])
print(table[-(2 ** 127)])
print(len(table))

# Membership.
print(18446744073709551616 in (1 << 64, 2))

# f-string rendering.
print(f"{big_dec}")
print(f"{0xDEADBEEFDEADBEEFDEADBEEF}")
print(f"dec={123456789012345678901234567890123456789} hex={0xFFFF_FFFF_FFFF_FFFF_FFFF:x}")

# str()/repr of literal values.
print(str(170141183460469231731687303715884105727))
print(repr(-99999999999999999999999999))

# Literals inside function bodies re-materialize per call.
def offset(n):
    return n + 123456789012345678901234567890123456789


for i in range(3):
    print(offset(i))
