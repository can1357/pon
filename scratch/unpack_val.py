try:
    a, b = [1, 2, 3]
except ValueError as exc:
    print("ok", type(exc).__name__)
try:
    it = iter(object())
except TypeError as exc:
    print("obj", exc)
