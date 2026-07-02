m = map(lambda x: x * 2, [1, 2, 3])
print(next(m), list(m))
print(list(filter(None, [0, 1, 0, 2])))
print(list(zip([1, 2], ["a", "b"])))

longer = zip([1], [2, 3], strict=True)
print(next(longer))
try:
    print(next(longer))
except Exception as exc:
    print(str(exc))

shorter = zip([1, 2], [3], strict=True)
print(next(shorter))
try:
    print(next(shorter))
except Exception as exc:
    print(str(exc))
