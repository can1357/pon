l = [3,1,[2]]
c = l.copy()
print(c, c is l, c[2] is l[2])
l.append(9); print(c)
print([].copy())
print(list.copy([4,5]))
import sys
a = sys.argv.copy(); print(type(a).__name__)
try:
    [1].copy(2)
except TypeError as e:
    print('TE', 'takes no arguments' in str(e))
