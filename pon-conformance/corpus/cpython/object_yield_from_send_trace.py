# Derived from CPython v3.14.0 Lib/test/test_yield_from.py topics (PSF license).

trace = []


def inner():
    trace.append("inner start")
    value = yield "inner one"
    trace.append("inner got " + str(value))
    value = yield "inner two"
    trace.append("inner got " + str(value))
    trace.append("inner finish")


def outer():
    trace.append("outer start")
    value = yield "outer first"
    trace.append("outer got " + str(value))
    yield from inner()
    value = yield "outer last"
    trace.append("outer got " + str(value))
    trace.append("outer finish")


gen = outer()
trace.append("yielded " + next(gen))
value = 1
try:
    while True:
        yielded = gen.send(value)
        trace.append("yielded " + yielded)
        value = value + 1
except StopIteration:
    trace.append("stopped")

print(trace)
