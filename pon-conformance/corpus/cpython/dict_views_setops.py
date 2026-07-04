# dict view re-iteration, set operations, equality, and live mutation.


def show_set(label, value):
    print(label, sorted(value))


d = {"a": 1, "b": 2}
keys = d.keys()
values = d.values()
items = d.items()

print("types", type(keys).__name__, type(values).__name__, type(items).__name__)
print("keys iter", sorted(keys), sorted(keys))
print("items iter", sorted(items), sorted(items))
print("values iter", sorted(values), sorted(values))
print("len in", len(keys), "a" in keys, 2 in values, ("b", 2) in items)

show_set("keys minus", keys - {"a"})
show_set("keys and", keys & {"b", "c"})
show_set("keys or", keys | {"c"})
show_set("keys xor", keys ^ {"b", "c"})
show_set("set minus keys", {"a", "c"} - keys)
show_set("set and keys", {"b", "c"} & keys)
show_set("set or keys", {"c"} | keys)
show_set("set xor keys", {"b", "c"} ^ keys)

show_set("items minus", items - {("a", 1)})
show_set("items and", items & {("b", 2), ("c", 3)})
show_set("items or", items | {("c", 3)})
show_set("items xor", items ^ {("b", 2), ("c", 3)})
show_set("set minus items", {("a", 1), ("c", 3)} - items)
show_set("set and items", {("b", 2), ("c", 3)} & items)
show_set("set or items", {("c", 3)} | items)
show_set("set xor items", {("b", 2), ("c", 3)} ^ items)

print("equality", keys == {"a", "b"}, items == {("a", 1), ("b", 2)}, values == d.values(), values == values)

d["c"] = 3
d["a"] = 10
print("mutated keys", sorted(keys))
print("mutated values", sorted(values))
print("mutated items", sorted(items))

try:
    values & {1}
except TypeError as exc:
    print("values and", type(exc).__name__)
