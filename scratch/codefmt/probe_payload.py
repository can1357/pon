import enum

class Color(enum.IntEnum):
    RED = 1

class MyInt(int):
    pass

class TupInt(int):
    def __new__(cls, a, b):
        return super().__new__(cls, a + b)

t = TupInt(3, 4)
print("T1", int(t), t + 1)
m = MyInt(7)
print("M1", int(m), m + 1)
print("C1", Color.RED + 1, int(Color.RED))
print("C2", hex(Color.RED))
try:
    print("C3", Color.RED.__format__('03d'))
except Exception as e:
    print("C3-ERR", type(e).__name__, e)
