d = {}
d.update((('A', 1), ('B', 2)))
d.update((name, i) for i, name in enumerate(['C','D']))
class M:
    def keys(self): return ['k1','k2']
    def __getitem__(self, k): return 'v-'+k
d.update(M())
d.update()
print(sorted(d.items()))
g = globals()
g.update((('X', 10),))
print('X =', X)
try:
    d.update([1,2])
except TypeError as e:
    print('TypeError:', e)
try:
    d.update([(1,2,3)])
except ValueError as e:
    print('ValueError:', e)
