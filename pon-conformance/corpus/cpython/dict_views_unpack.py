d = {"b": 2, "a": 1, "c": 3}
for k, v in d.items():
    print(k, v)
pairs = [(k, v) for k, v in d.items()]
print(pairs)
print(list(d.items()))
print(sorted(d.items()))
print(list(d.keys()))
print(sorted(d.keys()))
print(list(d.values()))
print(sorted(d.values()))
rebuilt = dict(d.items())
print(rebuilt)
print(sorted(rebuilt.items()))
print(dict(sorted(d.items())))
nested = {"x": (1, 2), "y": (3, 4)}
for (k, (a, b)) in nested.items():
    print(k, a, b)
print([a + b for _, (a, b) in nested.items()])
total = 0
for _, v in d.items():
    total += v
print(total)
