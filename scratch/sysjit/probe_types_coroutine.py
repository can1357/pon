import types
@types.coroutine
def async_yield(v):
    return (yield v)
print("COROUTINE_DECORATOR_OK")
