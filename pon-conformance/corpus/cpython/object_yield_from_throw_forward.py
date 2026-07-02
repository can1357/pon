# Derived from CPython v3.14.0 Lib/test/test_yield_from.py topics (PSF license).

class LunchError(Exception):
    pass


trace = []


def inner():
    try:
        trace.append("inner start")
        yield "inner one"
        yield "inner two"
    except LunchError as exc:
        trace.append("inner caught " + str(exc))
        yield "inner saved"
        yield "inner after"
    finally:
        trace.append("inner finally")


def outer():
    try:
        trace.append("outer start")
        yield "outer first"
        yield from inner()
        yield "outer last"
    finally:
        trace.append("outer finally")


gen = outer()
print(next(gen))
print(next(gen))
print(gen.throw(LunchError("tomato")))
print(next(gen))
print(next(gen))
try:
    next(gen)
except StopIteration:
    print("stopped")
else:
    print("not stopped")
print(trace)
