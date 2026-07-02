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
