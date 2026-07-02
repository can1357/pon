a = [1, 2]
b = (3, 4)

# tuple displays: leading / trailing / middle / multiple stars
print((*a, 9))
print((9, *a))
print((8, *a, 9))
print((*a, *b))
print((*a, 0, *b, *a))
print((*a,))
print((*(), *a, *[]))
print(())

# nested starred display feeding another starred display
inner = (*a, *b)
print((*inner, *a), len((*inner,)))

# list displays mirror the same staging path
print([*a, 9], [9, *a], [*a, *b])
print([*(), *[]], [*a])

# set displays: elements added as iterated; print sorted for determinism
print(sorted({*a, 9}))
print(sorted({9, *a}))
print(sorted({*a, *b}))
print(sorted({*a, *a, 1}))
print(sorted({*(), *a}))
print(sorted({*b}))

# empty iterables in every position
empty = []
print((*empty,), [*empty], sorted({*empty, 7}))

# starred generators (consumed exactly once, in display order)
def gen():
    yield 10
    yield 11

print((*gen(), 12))
print([13, *gen()])
print(sorted({*gen(), 10}))
g = (x * x for x in range(3))
print((*g,))
print((*g,))  # exhausted generator contributes nothing

# star of str / range / dict (keys) / dict.keys() / dict.values()
s = "abc"
d = {"k1": 1, "k2": 2}
print((*s, "d"))
print([*range(4), *s])
print(sorted({*range(3), *"aa"}, key=str))
print((*d,))
print((*d.keys(), *d.values()))
print([*d, *d.keys()])
print(sorted({*d, "k1"}))

# evaluation order: stars are expanded at their display position
log = []
def tag(x):
    log.append(x)
    return [x]

t = (*tag(1), *tag(2), 5, *tag(3))
print(t, log)

# single-element and deeply mixed shapes
print((*[42],), (*"z",), (*range(1),))
print((1, *[2, 3], (4, *b)))
