import gc


class Meta(type):
    @classmethod
    def __prepare__(mcls, name, bases, **kw):
        ns = {}
        ns["seeded"] = ["prepared", "mapping", "values"]
        return ns

    def __new__(mcls, name, bases, ns, **kw):
        gc.collect()
        gc.collect()
        junk = [list(range(64)) for _ in range(64)]
        gc.collect()
        del junk
        return super().__new__(mcls, name, bases, ns)


class C(metaclass=Meta):
    payload = [1, 2, 3]

    def describe(self):
        return "payload=%r seeded=%r" % (self.payload, self.seeded)


print(C().describe())
