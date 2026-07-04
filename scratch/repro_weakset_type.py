from _weakrefset import WeakSet
async def _coro(): pass
coro = _coro()
coroutine = type(coro)
coro.close()
s = WeakSet()
print("before")
s.add(coroutine)
print("after", list(s))
