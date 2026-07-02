# CPython-facing generator handler-stack conformance cases.

class Boom(Exception):
    pass


def ordinary_send(trace):
    trace.append("send:start")
    try:
        value = yield "send:ready"
        trace.append("send:got:" + str(value))
        yield "send:after"
    finally:
        trace.append("send:finally")


def throw_nested(trace):
    trace.append("throw:start")
    try:
        try:
            trace.append("throw:inner")
            value = yield "throw:ready"
            trace.append("throw:got:" + str(value))
        except Boom as exc:
            trace.append("throw:caught:" + str(exc))
            value = yield "throw:handled"
            trace.append("throw:after-handled:" + str(value))
        yield "throw:done"
    finally:
        trace.append("throw:finally")


def close_once(trace):
    trace.append("close:start")
    try:
        yield "close:ready"
        trace.append("close:after")
    finally:
        trace.append("close:finally")


def nested_handlers(trace):
    trace.append("nest:start")
    try:
        try:
            trace.append("nest:inner")
            value = yield "nest:first"
            trace.append("nest:got:" + str(value))
            try:
                yield "nest:second"
            finally:
                trace.append("nest:inner-finally")
            yield "nest:third"
        except Boom as exc:
            trace.append("nest:caught:" + str(exc))
            value = yield "nest:handled"
            trace.append("nest:after-handled:" + str(value))
        finally:
            trace.append("nest:middle-finally")
        yield "nest:after"
    finally:
        trace.append("nest:outer-finally")


def stop(label, gen):
    try:
        print(label, next(gen))
    except StopIteration:
        print(label, "StopIteration")
    else:
        print(label, "not-stopped")


trace = []
gen = ordinary_send(trace)
print("send next", next(gen))
print("send send", gen.send(7))
stop("send stop", gen)
print("send trace", trace)

trace = []
gen = throw_nested(trace)
print("throw next", next(gen))
print("throw throw", gen.throw(Boom("boom")))
print("throw send", gen.send("resume"))
stop("throw stop", gen)
print("throw trace", trace)

trace = []
gen = close_once(trace)
print("close next", next(gen))
print("close close", gen.close())
print("close again", gen.close())
stop("close stop", gen)
print("close trace", trace)

trace = []
gen = nested_handlers(trace)
print("nest next", next(gen))
print("nest send", gen.send("alpha"))
print("nest throw", gen.throw(Boom("beta")))
print("nest send2", gen.send("omega"))
stop("nest stop", gen)
print("nest trace", trace)
