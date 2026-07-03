# Frame-root windows: objects held only by function locals must survive a
# gc.collect() issued inside the frame, a deleted local must finalize once
# the collector runs, and class construction must keep the class-body
# namespace alive while metaclass hooks trigger collections.
import gc


FINALIZED = []


class Tracked:
    def __del__(self):
        FINALIZED.append("finalized")


def locals_survive_collect():
    x = [1, 2, 3]
    y = {"a": 1, "b": 2}
    s = "pon" * 5
    t = (x, "fixed")
    gc.collect()
    gc.collect()
    return x[0] + x[2] + y["b"], s[:3], t[1]


def del_then_collect():
    obj = Tracked()
    del obj
    gc.collect()
    gc.collect()
    return list(FINALIZED)


# Bind results before printing: a literal evaluated before a collecting call
# is an expression temporary, and temporaries (unlike named locals) are not
# frame-rooted across a collection.
survivors = locals_survive_collect()
print("locals", survivors)
finalized = del_then_collect()
print("del-local", finalized)


class Meta(type):
    @classmethod
    def __prepare__(mcls, name, bases, **kw):
        # Allocate-heavy prepared mapping: the body executes into this dict,
        # and its values are reachable only through the class machinery while
        # the hooks below run collections.
        ns = {}
        for i in range(64):
            ns["pad%d" % i] = [i] * 8
        return ns

    def __new__(mcls, name, bases, ns, **kw):
        gc.collect()
        pressure = []
        for i in range(64):
            pressure.append([i] * 16)
        gc.collect()
        del pressure
        cleaned = {}
        for key in ns:
            if not key.startswith("pad"):
                cleaned[key] = ns[key]
        gc.collect()
        return super().__new__(mcls, name, bases, cleaned)

    def __init__(cls, name, bases, ns, **kw):
        gc.collect()
        super().__init__(name, bases, ns)


class Payload(metaclass=Meta):
    stamp = ["s", "t"]

    def get(self):
        return self.stamp + [type(self).__name__]


print("class", Payload().get())
print("pads dropped", sum(1 for k in Payload.__dict__ if k.startswith("pad")))
print("prepared kept", Payload.stamp)
