def f(a=0, b=0, **kw): return (a, b, dict(sorted(kw.items())))
class Obj: pass
o = Obj(); o.a = 1; o.b = 2
print(f(**o.__dict__))
class M:
    def keys(self): return ['a','x']
    def __getitem__(self, k): return {'a':10,'x':20}[k]
print(f(**M()))
print(f(**{'b':5}))
try:
    f(**[1,2])
except TypeError as e:
    print('TE1', e)
try:
    f(**{1:2})
except TypeError as e:
    print('TE2', e)
try:
    f(a=1, **{'a':2})
except TypeError as e:
    print('TE3', 'multiple values' in str(e))
d = {'a':1}
d.update([('b',2)])
print(sorted(d.items()))
