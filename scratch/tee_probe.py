from itertools import tee
print(repr(tee(5, 0)))
try: tee(5)
except TypeError as e: print('E1', e)
try: tee([], 'x')
except TypeError as e: print('E2', e)
try: tee([], -1)
except ValueError as e: print('E3', e)
a,b = tee([1,2,3])
print(type(a).__name__)
print(next(a), next(a), next(b), next(a), list(a), list(b))
# interleaved over a generator (single consumption, side effects once)
def gen():
    for i in range(4):
        print('pull', i)
        yield i
x, y, z = tee(gen(), 3)
print(next(x), next(y), next(x), next(z), next(z), next(y))
print(list(x), list(y), list(z))
# lazy: infinite source
from itertools import count, islice
c1, c2 = tee(count())
print(list(islice(c1, 3)), list(islice(c2, 5)), list(islice(c1, 3)))
try: tee()
except TypeError as e: print('E5', e)
