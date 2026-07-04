# object is a universal base for runtime values.


class Carrier:
    marker = staticmethod(lambda: None)

    def method(self):
        return None


class Instance:
    pass


def generator():
    yield 1


values = [
    Carrier().method,
    len,
    iter([1]),
    Carrier.__dict__["marker"],
    generator(),
    Instance(),
    1,
    "x",
    None,
]

for value in values:
    print(isinstance(value, object))
print(issubclass(type({}.get), object))
