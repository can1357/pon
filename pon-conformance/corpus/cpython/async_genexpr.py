# PEP 525 async generators: generator expressions and `async def` + `yield`,
# manually driven.
#
# No event loop: awaitables are plain `__await__` generators, coroutines are
# stepped with `send`, and each `__anext__()`/`asend()`/`athrow()`/`aclose()`
# awaitable is driven to `StopIteration` (one async-yield step) or
# `StopAsyncIteration` (exhaustion) — the same manual-driving discipline as
# async_comprehensions.py.


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


def drive(coro, replies=()):
    """Step a coroutine to completion, printing each pause."""
    replies = iter(replies)
    payload = None
    while True:
        try:
            signal = coro.send(payload)
        except StopIteration as exc:
            return exc.value
        print("pause:", signal)
        payload = next(replies, None)


def anext_step(agen):
    """Drive one non-pausing __anext__() awaitable by hand."""
    aw = agen.__anext__()
    try:
        aw.send(None)
    except StopIteration as exc:
        return ("yield", exc.value)
    except StopAsyncIteration:
        return ("done", None)
    return ("pause", None)


print("== async genexpr, manually driven ==")

gen = (x * 10 async for x in AsyncSeq(range(3)))
print(type(gen).__name__)
print(gen.__aiter__() is gen)
for _ in range(4):
    print(anext_step(gen))

print("== async genexpr consumed by async for ==")


async def consume():
    out = []
    async for item in (x + 100 async for x in AsyncSeq((1, 2, 3)) if x != 2):
        out.append(item)
    return out


print(drive(consume()))

print("== genexpr made async by an awaited element ==")


async def await_elt():
    total = 0
    async for got in (await Ready(n * 3) for n in range(4)):
        total += got
    return total


print(drive(await_elt()))

print("== async genexpr chained through another async genexpr ==")


async def chained():
    inner = (x + 1 async for x in AsyncSeq((10, 20, 30)))
    return [y async for y in (2 * x async for x in inner)]


print(drive(chained()))

print("== async generator function: yields and awaits ==")


async def agen_fn(seq):
    async for x in AsyncSeq(seq):
        got = await Ready(x)
        yield ("item", got)
    yield ("tail", None)


async def collect():
    return [item async for item in agen_fn("ab")]


print(drive(collect()))

print("== suspension inside the async generator passes through ==")


async def pausing_gen():
    got = await PauseOnce("a")
    yield got
    got = await PauseOnce("b")
    yield got


ag = pausing_gen()
aw = ag.__anext__()
print("passthrough:", aw.send(None))
try:
    aw.send("r1")
except StopIteration as exc:
    print("yield:", exc.value)
aw = ag.__anext__()
print("passthrough:", aw.send(None))
try:
    aw.send("r2")
except StopIteration as exc:
    print("yield:", exc.value)
aw = ag.__anext__()
try:
    aw.send(None)
except StopAsyncIteration:
    print("exhausted")

print("== asend delivers a value to the paused yield ==")


async def echoing():
    got = "start"
    while True:
        got = yield got


ag = echoing()
print(anext_step(ag))
aw = ag.asend("ping")
try:
    aw.send(None)
except StopIteration as exc:
    print("asend:", exc.value)

print("== non-None send into a just-started async generator ==")

ag2 = echoing()
aw = ag2.asend("early")
try:
    aw.send(None)
except TypeError as exc:
    print("TypeError:", exc)

print("== athrow: caught and uncaught ==")


async def catching():
    try:
        yield "before"
    except ValueError as exc:
        yield ("caught", str(exc))


ag = catching()
print(anext_step(ag))
aw = ag.athrow(ValueError("boom"))
try:
    aw.send(None)
except StopIteration as exc:
    print("athrow:", exc.value)


async def plain():
    yield 1


ag = plain()
print(anext_step(ag))
aw = ag.athrow(KeyError("missing"))
try:
    aw.send(None)
except KeyError as exc:
    print("propagated:", exc.args)

print("== aclose: finally runs, generator becomes exhausted ==")


async def closable():
    try:
        yield 1
        yield 2
    finally:
        print("finally ran")


ag = closable()
print(anext_step(ag))
aw = ag.aclose()
try:
    aw.send(None)
except StopIteration as exc:
    print("aclose:", exc.value)
print(anext_step(ag))

print("== aclose of an unstarted async generator ==")


async def tiny():
    yield 1


ag = tiny()
aw = ag.aclose()
try:
    aw.send(None)
except StopIteration as exc:
    print("aclose-unstarted:", exc.value)

print("== yielding while closing is a RuntimeError ==")


async def ignorer():
    try:
        yield 1
    finally:
        yield 2


ag = ignorer()
print(anext_step(ag))
aw = ag.aclose()
try:
    aw.send(None)
except RuntimeError as exc:
    print("RuntimeError:", exc)

print("== an awaited __anext__() cannot be awaited twice ==")

ag = tiny()
aw = ag.__anext__()
try:
    aw.send(None)
except StopIteration as exc:
    print("yield:", exc.value)
try:
    aw.send(None)
except RuntimeError:
    print("reuse RuntimeError")

print("== PEP 525: escaping StopIteration / StopAsyncIteration ==")


async def raises_stop():
    yield 1
    raise StopIteration(9)


ag = raises_stop()
print(anext_step(ag))
aw = ag.__anext__()
try:
    aw.send(None)
except RuntimeError as exc:
    print("converted:", exc)


async def raises_stop_async():
    yield 1
    raise StopAsyncIteration


ag = raises_stop_async()
print(anext_step(ag))
aw = ag.__anext__()
try:
    aw.send(None)
except RuntimeError as exc:
    print("converted:", exc)

print("== async genexpr in a sync function ==")


def make_gen(seq):
    return (x - 1 async for x in AsyncSeq(seq))


async def consume_made():
    return [x async for x in make_gen((7, 8))]


print(drive(consume_made()))

print("== done ==")
