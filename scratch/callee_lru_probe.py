import functools
calls = []
@functools.lru_cache()
def f(x=0):
    calls.append(x)
    print('inside', x)
    return x + 1
print('f_type', type(f), repr(f), 'call_attr', getattr(f, '__call__', None), 'wrapped', getattr(f, '__wrapped__', None))
try:
    print('result0', f())
    print('result1', f(2))
    print('result2', f(2))
    print('calls', calls)
except BaseException as e:
    print('error', type(e).__name__, str(e))
    raise
