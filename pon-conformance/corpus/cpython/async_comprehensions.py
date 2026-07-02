# PEP 530 async comprehensions and `async for`, manually driven.
#
# No event loop: every coroutine is stepped with `send`, awaitables are
# plain `__await__` generators, and exhaustion is signaled by raising
# StopAsyncIteration from inside the awaited `__anext__` result — the same
# manual-driving discipline as async_manual_send.py.


def _resolve(value):
    return value
    yield


def _pause(value):
    got = yield ("item", value)
    return got


class StopNow:
    def __await__(self):
        raise StopAsyncIteration
        yield


class Ready:
    def __init__(self, value):
        self.value = value

    def __await__(self):
        return _resolve(self.value)


class PauseOnce:
    def __init__(self, value):
        self.value = value

    def __await__(self):
        return _pause(self.value)


class AsyncSeq:
    """Async iterator over a sequence; each __anext__ resolves immediately."""

    def __init__(self, seq):
        self.seq = list(seq)
        self.i = 0

    def __aiter__(self):
        return self

    def __anext__(self):
        if self.i >= len(self.seq):
            return StopNow()
        value = self.seq[self.i]
        self.i += 1
        return Ready(value)


class PausingSeq:
    """Async iterator that suspends the driving coroutine once per item."""

    def __init__(self, seq):
        self.seq = list(seq)
        self.i = 0

    def __aiter__(self):
        return self

    def __anext__(self):
        if self.i >= len(self.seq):
            return StopNow()
        value = self.seq[self.i]
        self.i += 1
        return PauseOnce(value)


class FreshIter:
    """__aiter__ returns a distinct iterator object, not self."""

    def __init__(self, seq):
        self.seq = seq

    def __aiter__(self):
        return AsyncSeq(self.seq)


def drive(coro, replies=()):
    """Step a coroutine to completion, printing each pause."""
    replies = iter(replies)
    payload = None
    while True:
        try:
            paused = coro.send(payload)
        except StopIteration as exc:
            return exc.value
        print("paused:", paused)
        payload = next(replies, None)


print("== async list comprehension ==")


async def basic_list():
    return [x * 10 async for x in AsyncSeq(range(4))]


print(drive(basic_list()))

print("== async set / dict comprehensions ==")


async def set_and_dict():
    s = {x % 3 async for x in AsyncSeq(range(7))}
    d = {k: k + k async for k in AsyncSeq("ab")}
    return sorted(s), d


print(drive(set_and_dict()))

print("== filters: sync and awaited conditions ==")


async def filtered():
    evens = [n async for n in AsyncSeq(range(6)) if n % 2 == 0]
    truthy = [n for n in range(5) if await Ready(n % 2)]
    return evens, truthy


print(drive(filtered()))

print("== await in the element ==")


async def elt_await():
    return [await Ready(n * 3) for n in range(4)]


print(drive(elt_await()))

print("== multi-clause: async outer, sync inner ==")


async def async_then_sync():
    return [(x, y) async for x in AsyncSeq("ab") for y in (1, 2) if y != 1 or x != "b"]


print(drive(async_then_sync()))

print("== multi-clause: sync outer, async inner ==")


async def sync_then_async():
    return [(x, y) for x in "ab" async for y in AsyncSeq((1, 2))]


print(drive(sync_then_async()))

print("== nested async comprehension in async comprehension ==")


async def nested_async():
    return [[y * 2 async for y in AsyncSeq(range(x))] async for x in AsyncSeq((1, 3))]


print(drive(nested_async()))

print("== implicitly async outer comprehension ==")


async def implicit_outer():
    return [[y + x async for y in AsyncSeq((10, 20))] for x in (1, 2)]


print(drive(implicit_outer()))

print("== suspension inside a comprehension ==")


async def pausing_comp():
    return [got async for got in PausingSeq("xyz")]


print("final:", drive(pausing_comp(), ["r1", "r2", "r3"]))

print("== async for statement ==")


async def afor_total():
    total = 0
    async for x in AsyncSeq((1, 2, 3, 4)):
        total += x
    return total


print(drive(afor_total()))

print("== async for over a fresh __aiter__ ==")


async def afor_fresh():
    out = []
    async for x in FreshIter(("p", "q")):
        out.append(x)
    return out


print(drive(afor_fresh()))

print("== async for: continue / break / else ==")


async def afor_control():
    seen = []
    async for x in AsyncSeq(range(6)):
        if x == 1:
            continue
        if x == 4:
            break
        seen.append(x)
    else:
        seen.append("no-else")
    async for y in AsyncSeq(range(2)):
        seen.append(("second", y))
    else:
        seen.append("else-ran")
    return seen


print(drive(afor_control()))

print("== async for: early return ==")


async def afor_return():
    async for x in AsyncSeq((5, 6, 7)):
        if x == 6:
            return ("early", x)
    return "exhausted"


print(drive(afor_return()))

print("== async for: suspension in the statement ==")


async def afor_pausing():
    got = []
    async for x in PausingSeq((1, 2)):
        got.append(x)
    return got


print("final:", drive(afor_pausing(), ["a", "b"]))

print("== StopAsyncIteration from the body propagates ==")


async def body_raises():
    async for x in AsyncSeq((1, 2)):
        raise StopAsyncIteration("from-body")
    return "not-reached"


try:
    drive(body_raises())
    print("no raise")
except StopAsyncIteration as exc:
    print("propagated:", exc.args)

print("== missing __aiter__ is a TypeError ==")


async def not_async_iterable():
    async for _ in 42:
        pass


try:
    drive(not_async_iterable())
    print("no raise")
except TypeError as exc:
    print("TypeError:", exc)

print("== exception from __anext__ propagates ==")


class ExplodingIter:
    def __init__(self):
        self.i = 0

    def __aiter__(self):
        return self

    def __anext__(self):
        self.i += 1
        if self.i > 2:
            raise ValueError("anext-boom")
        return Ready(self.i)


async def anext_raises():
    out = []
    async for x in ExplodingIter():
        out.append(x)
    return out


try:
    drive(anext_raises())
    print("no raise")
except ValueError as exc:
    print("ValueError:", exc)

print("== done ==")
