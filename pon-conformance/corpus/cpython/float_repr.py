# Float repr/str canaries for exact surface compatibility.

values = [0.1, 1e16, 1e-5, 9007199254740993.0, float("inf"), float("-inf"), float("nan"), -0.0]

for value in values:
    print(repr(value), str(value))

print("canaries", repr(0.1), repr(1e16), repr(1e-5))
print("large-int-float", repr(9007199254740993.0), int(9007199254740993.0))
print("inf-arith", repr(float("inf") + 1.0), repr(float("-inf") * 2.0))
print("nan-check", repr(float("nan")), float("nan") == float("nan"))
print("neg-zero", repr(-0.0), str(-0.0), -0.0 == 0.0)
