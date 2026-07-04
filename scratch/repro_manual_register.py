from abc import ABCMeta, abstractmethod
async def _coro(): pass
coro = _coro(); coroutine = type(coro); coro.close()
class Awaitable(metaclass=ABCMeta):
    @abstractmethod
    def __await__(self): yield
    @classmethod
    def __subclasshook__(cls, C): return NotImplemented
class Coroutine(Awaitable):
    @abstractmethod
    def send(self, value): raise StopIteration
    @abstractmethod
    def throw(self, typ, val=None, tb=None): raise typ
    def close(self): pass
    @classmethod
    def __subclasshook__(cls, C): return NotImplemented
print("registry", Coroutine._abc_registry)
print("add")
Coroutine._abc_registry.add(coroutine)
print("inc")
ABCMeta._abc_invalidation_counter += 1
print("done")
