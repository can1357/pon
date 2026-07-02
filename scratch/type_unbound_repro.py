f = dict.__setitem__
d = {}
f(d, 'k', 1)
print(d)
print(dict.__getitem__(d, 'k'))
print(dict.get(d, 'k'))
lst = []
list.append(lst, 5)
print(lst)
