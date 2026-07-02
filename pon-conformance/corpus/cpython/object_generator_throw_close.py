# Derived from CPython v3.14.0 Lib/test/test_generators.py topics (PSF license).

class Signal(Exception):
    pass


trace = []


def catcher():
    trace.append("start")
    try:
        while True:
            try:
                value = yield "ready"
                trace.append("got " + str(value))
            except Signal as exc:
                trace.append("caught " + str(exc))
                yield "handled"
    finally:
        trace.append("finally")


gen = catcher()
print(next(gen))
print(gen.send("one"))
print(gen.throw(Signal("boom")))
print(gen.send("two"))
print(gen.close())
print(trace)

try:
    next(gen)
except StopIteration as exc:
    print(type(exc).__name__)
else:
    print("not stopped")
