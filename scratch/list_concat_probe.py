class L(list): pass
a = [1] + L([2])
print(a, type(a).__name__)
b = L([1]) + [2]
print(b, type(b).__name__)
c = [1]; c += (2,)
print(c)
d = [1]; d += {'k': 1}.keys()
print(d)
e = [1]; e += (x for x in [3])
print(e)
try:
    [1] + (2,)
except TypeError as ex:
    print('TE', ex)
a2 = [1]; print(a2 + a2)
sub = L([9]); sub += (8,); print(list(sub), type(sub).__name__)
try:
    z=[1]; z += 5
except TypeError as e:
    print('TE2', type(e).__name__)
