try:
    dict.__setitem__([], 'k', 1)
except TypeError as e:
    print("A", type(e).__name__, e)
try:
    dict.get([], 'k')
except TypeError as e:
    print("B", type(e).__name__, e)
try:
    dict.update([], {})
except TypeError as e:
    print("C", type(e).__name__, e)
try:
    dict.__contains__([], 1)
except TypeError as e:
    print("D", type(e).__name__, e)
try:
    list.append({}, 1)
except TypeError as e:
    print("E", type(e).__name__, e)
try:
    dict.__setitem__({}, 'k')
except TypeError as e:
    print("F", type(e).__name__, e)
try:
    dict.__setitem__()
except TypeError as e:
    print("G", type(e).__name__, e)
try:
    dict.get()
except TypeError as e:
    print("H", type(e).__name__, e)
try:
    dict.__eq__([], {})
except TypeError as e:
    print("I", type(e).__name__, e)
d = {}
print(dict.__setitem__ is dict.__setitem__)
u = dict.update
u(d, {'a': 1})
m = d.update
m({'b': 2})
print(d)
print(dict.__setitem__.__name__, list.append.__name__)
print(dict.__contains__(d, 'a'), dict.__contains__(d, 'zz'))
print(dict.__len__(d), dict.__getitem__(d, 'a'))
dict.__delitem__(d, 'a')
print(d, sorted(dict.keys(d)))
print(dict.pop(d, 'b'), d)
