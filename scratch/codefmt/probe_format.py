import enum

class Color(enum.IntEnum):
    RED = 1
    GREEN = 2

class Perm(enum.IntFlag):
    R = 4
    W = 2

class MyInt(int):
    pass

print("A", repr(Color.RED))
print("B", int(Color.RED))
try:
    print("C", format(Color.RED, '03d'))
except Exception as e:
    print("C-ERR", type(e).__name__, e)
try:
    print("D", format(Perm.R, 'x'))
except Exception as e:
    print("D-ERR", type(e).__name__, e)
try:
    print("E", format(True, 'd'))
except Exception as e:
    print("E-ERR", type(e).__name__, e)
try:
    print("F", format(MyInt(7), '05d'))
except Exception as e:
    print("F-ERR", type(e).__name__, e)
print("G", format(Color.RED))
fmt = int.__format__
print("H", type(fmt).__name__)
