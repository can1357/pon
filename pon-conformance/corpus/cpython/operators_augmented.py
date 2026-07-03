value = 10
value += 5
value *= 2
value //= 3
value %= 7
print(value)
print(5 & 3, 5 | 2, 5 ^ 1, 1 << 4, 16 >> 2)
print(3 < 4 <= 4, 5 != 6, not False)

class ReflectedPow:
    def __rpow__(self, other):
        return ("rpow", other)


class ReflectedMatmul:
    def __rmatmul__(self, other):
        return ("rmatmul", other)


print(2 ** ReflectedPow())
print(1 @ ReflectedMatmul())
