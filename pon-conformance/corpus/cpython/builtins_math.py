class Indexy:
    def __index__(self):
        return 5

class Formattable:
    def __format__(self, spec):
        return "formatted"

print(divmod(5, 2), divmod(-5, 2))
print(pow(2, 3), pow(2, 3, 5), pow(2, -1, 5), pow(2, 3, -5))
print(round(2.675, 2) == 2.67, round(0.5) == 0, round(-0.5) == 0, round(2.5) == 2)
print(round(1234, -2), round(25, -1), round(15, -1))
print(bin(10), oct(10), hex(-10), bin(True), hex(Indexy()))
print(format(12, "04d"), format(3.5, ".1f"), format("hi", ">4s"), format(Formattable(), "ok"))
print(chr(9731), ord("☃"))
try:
    print(pow(2, -1, 4))
except Exception as exc:
    print(str(exc))


def err(fn):
    try:
        return repr(fn())
    except BaseException as exc:
        return type(exc).__name__ + ": " + str(exc)


# sum(): Neumaier compensation for floats (gh-100425) and typed-path
# transitions int -> float -> generic.
print(sum([0.1] * 10), sum([0.1] * 9, 0.1), sum([1e100, 1.0, -1e100, 1.0]))
print(sum([1, 0.5, 2]), sum([True, 0.5]), sum([True, True], False), sum([]))
print(sum([0.5, 2**62]), sum([2**62, 2**62, 0.125]), sum([10**40, 10**40]))
print(sum([float("inf"), float("inf")]), sum([1e308, 1e308]), sum(range(100), 10**30))
print(err(lambda: sum([], "x")))
print(err(lambda: sum([], b"")))
print(err(lambda: sum([], bytearray())))

# 3.14 unified division-by-zero shapes (gh-87999), all operand kinds.
print(err(lambda: 1 // 0))
print(err(lambda: 1 % 0))
print(err(lambda: 1.5 / 0.0))
print(err(lambda: 1.5 // 0.0))
print(err(lambda: 1.5 % 0.0))
print(err(lambda: divmod(1, 0)))
print(err(lambda: divmod(1.5, 0.0)))
print(err(lambda: 0**-1))
print(err(lambda: 0.0**-1))
print(err(lambda: 1j / 0))
print(err(lambda: 0j**-1))
