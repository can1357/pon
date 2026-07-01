scale = 3
mul = lambda value, offset=1: value * scale + offset
pairs = [(1, "b"), (3, "a"), (2, "c")]
print(mul(4), mul(4, 0))
print(sorted(pairs, key=lambda item: item[1]))
