def f(v, n): return v
d = dict.fromkeys(['x', 'y'], f)
print("fromkeys value is f:", d['x'] is f)
print("call:", d['x']('a', 'b'))
# also: dict.get returning the function then calling
print("get+call:", d.get('y')('c', 'd'))
