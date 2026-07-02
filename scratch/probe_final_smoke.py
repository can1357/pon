import contextlib
import gc
from collections import defaultdict, deque

# contextlib.ExitStack drives deque append/pop under the hood
order = []
with contextlib.ExitStack() as stack:
    stack.callback(order.append, 1)
    stack.callback(order.append, 2)
    stack.callback(order.append, 3)
print('exitstack', order)

# GC stress: many short-lived defaultdicts and deque contents survive collect
keep = defaultdict(list)
for i in range(200):
    t = defaultdict(list)
    t['x'].append(i)
    keep['sum'].append(t['x'][0])
d = deque(maxlen=50)
for i in range(500):
    d.append([i])
gc.collect()
print('gc ok', sum(keep['sum']) == sum(range(200)), len(d), d[0] if hasattr(d, '__getitem__') else list(d)[0], list(d)[-1])
