import array
from array import array as arr, ArrayType

# --- module surface ---------------------------------------------------------------
print(array.typecodes)
print(array.array is arr, ArrayType is array.array)
print(type(array.array("i")).__name__)

# --- construction: empty, list, tuple, bytes, generator, another array -------------
print(array.array("i"))
print(array.array("i", [1, 2, 3]))
print(array.array("b", (4, 5)))
print(array.array("B", b"\x01\x02\xff"))
print(array.array("H", bytes([1, 0, 2, 0])))
print(array.array("q", range(3)))
print(array.array("d", [0.5, 2.0, -1.25]))
print(array.array("f", [0.5, -2.5]))
print(array.array("i", array.array("i", [7, 8])))
print(array.array("d", array.array("i", [1, 2])))

# --- typecode/itemsize per code ------------------------------------------------------
for code in "bBhHiIlLqQfd":
    a = array.array(code, [0, 1])
    print(code, a.itemsize, a.typecode, a.tolist())

# --- len / getitem / negative indexing / setitem / delitem ---------------------------
a = array.array("i", [10, 20, 30, 40])
print(len(a), a[0], a[3], a[-1], a[-4])
a[1] = 99
a[-1] = -5
print(a.tolist())
del a[0]
print(a.tolist(), len(a))
try:
    a[10]
except IndexError as exc:
    print("IndexError:", exc)
try:
    a[-10] = 1
except IndexError as exc:
    print("IndexError:", exc)

# --- append / extend / insert / pop / remove / clear / reverse ------------------------
b = array.array("h")
b.append(1)
b.extend([2, 3])
b.extend(array.array("h", [4]))
print(b.tolist())
b.insert(0, -1)
b.insert(100, 9)
b.insert(-2, 7)
print(b.tolist())
print(b.pop(), b.pop(0), b.tolist())
b.remove(7)
print(b.tolist())
try:
    b.remove(1234)
except ValueError as exc:
    print("ValueError:", exc)
b.reverse()
print(b.tolist())
b.clear()
print(b.tolist(), len(b), bool(b), bool(array.array("i", [0])))
try:
    array.array("i", [1]).extend(array.array("h", [1]))
except TypeError as exc:
    print("TypeError:", exc)

# --- count / index / contains ----------------------------------------------------------
c = array.array("i", [1, 2, 1, 3, 1])
print(c.count(1), c.count(9), c.index(3), c.index(1, 1), c.index(1, 1, 3))
print(2 in c, 9 in c, True in array.array("b", [1]))
try:
    c.index(42)
except ValueError as exc:
    print("ValueError:", exc)

# --- tobytes / frombytes / tolist / fromlist round-trips ---------------------------------
d = array.array("i", [1, -1, 65536])
blob = d.tobytes()
e = array.array("i")
e.frombytes(blob)
print(d == e, e.tolist())
f = array.array("d")
f.fromlist([1.0, 2.5])
f.fromlist([])
print(f.tolist())
try:
    f.fromlist((1, 2))
except TypeError as exc:
    print("TypeError:", exc)
g = array.array("d", [3.5])
try:
    g.fromlist([1.0, "nope"])
except TypeError:
    print("fromlist rollback:", g.tolist())
try:
    array.array("i").frombytes(b"\x01\x02\x03")
except ValueError as exc:
    print("ValueError:", exc)

# --- iteration ---------------------------------------------------------------------------
total = 0
for value in array.array("l", [100, 200, 300]):
    total += value
print(total)
print(list(array.array("B", b"ab")), [x * 2 for x in array.array("i", [1, 2])])
it = iter(array.array("i", [5, 6]))
print(next(it), next(it))
try:
    next(it)
except StopIteration:
    print("StopIteration")

# --- equality: value-based, cross-typecode, foreign operands ------------------------------
print(array.array("i", [1, 2]) == array.array("i", [1, 2]))
print(array.array("i", [1, 2]) == array.array("d", [1.0, 2.0]))
print(array.array("b", [1]) == array.array("i", [1, 2]))
print(array.array("i", [1, 2]) != array.array("i", [2, 1]))
print(array.array("i", [1]) == [1], array.array("i") == array.array("d"))

# --- float behavior: f narrows to f32, d keeps f64 ----------------------------------------
fl = array.array("f", [1.1])
print(fl[0] == 1.1, abs(fl[0] - 1.1) < 1e-7)
dbl = array.array("d", [1.1])
print(dbl[0] == 1.1)
print(array.array("f", [2]).tolist(), array.array("d", [True]).tolist())

# --- integer range checks: per-typecode OverflowError texts --------------------------------
for code, bad in [("b", 128), ("b", -129), ("B", 256), ("B", -1),
                  ("h", 32768), ("H", -1), ("i", 2**31), ("I", -1)]:
    try:
        array.array(code, [bad])
    except OverflowError as exc:
        print(code, "OverflowError:", exc)
print(array.array("b", [-128, 127]).tolist(), array.array("B", [0, 255]).tolist())
print(array.array("q", [-2**63, 2**63 - 1]).tolist())
print(array.array("Q", [0]).tolist(), array.array("L", [2**63 - 1]).tolist())
try:
    array.array("L", [-1])
except OverflowError as exc:
    print("OverflowError:", exc)

# --- element type errors ---------------------------------------------------------------------
try:
    array.array("i", [1.5])
except TypeError as exc:
    print("TypeError:", exc)
try:
    array.array("i").append("x")
except TypeError as exc:
    print("TypeError:", exc)
try:
    array.array("d", ["nope"])
except TypeError as exc:
    print("TypeError:", exc)

# --- constructor errors ------------------------------------------------------------------------
try:
    array.array("x")
except ValueError as exc:
    print("ValueError:", exc)
try:
    array.array(3)
except TypeError as exc:
    print("TypeError:", exc)
try:
    array.array()
except TypeError:
    print("array() TypeError")
try:
    array.array("i", b"\x01\x02\x03")
except ValueError as exc:
    print("ValueError:", exc)
try:
    array.array("i", "text")
except TypeError as exc:
    print("TypeError:", exc)

# --- repr ----------------------------------------------------------------------------------------
print(repr(array.array("i")))
print(repr(array.array("i", [1, 2, 3])))
print(repr(array.array("d", [1.5])))
print(repr(array.array("b", [-1, 0, 1])))
print(str(array.array("H", [7])))
