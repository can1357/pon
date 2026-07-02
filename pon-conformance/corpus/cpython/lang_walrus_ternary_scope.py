# Derived from CPython v3.14.0 Lib/test/test_grammar.py topics (PSF license).

values = [1, 2, 3, 4]
picked = []
index = 0
while index < len(values) and (item := values[index]) < 4:
    picked.append(item)
    index += 1
print("while", picked, item, index)


def classify(seq):
    return "empty" if (size := len(seq)) == 0 else ("one" if size == 1 else "many-" + str(size))


print("classify", classify([]), classify([10]), classify([10, 20, 30]))
seen = "unset"
doubled = [seen for value in values if (seen := value * 2) > 4]
print("comprehension", doubled, seen)

calls = []


def take(value):
    calls.append(value)
    return value


first = "yes" if (found := take(0)) else "no"
second = "yes" if (found := take(5)) else "no"
print("ternary", first, second, found, calls)
