# Derived from CPython v3.14.0 Lib/test/test_with.py topics (PSF license).

events = []


class TupleManager:
    def __init__(self, label, value):
        self.label = label
        self.value = value

    def __enter__(self):
        events.append("enter " + self.label)
        return self.value

    def __exit__(self, exc_type, exc, traceback):
        if exc_type is None:
            kind = "none"
        else:
            kind = exc_type.__name__
        events.append("exit " + self.label + " " + kind)
        return False


with TupleManager("star", (1, 2, 3, 4)) as (first, *middle, last):
    print("star", first, middle, last)

target = {"slots": [0, 0, 0, 0]}
with TupleManager("subscript", ("a", "b", "c", "d")) as (
    target["slots"][0],
    target["slots"][1],
    target["slots"][2],
    target["slots"][3],
):
    print("slots", target["slots"])


class Holder:
    pass


holder = Holder()
with TupleManager("attrs", (7, 8)) as (holder.left, holder.right):
    print("attrs", holder.left, holder.right)

print("events", events)
