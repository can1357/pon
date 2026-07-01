data = {"a": 1, "b": 2}
print(data.get("a"), data.get("z", 9))
print(list(data.keys()))
print(list(data.values()))
print(list(data.items()))
print(data.setdefault("c", 3))
print(data.pop("b"))
data.update({"d": 4})
print(list(data.items()))
