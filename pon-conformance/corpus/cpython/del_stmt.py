class Box:
    pass


print("del statements")


def show_missing(label, thunk):
    try:
        thunk()
    except NameError:
        print(label, "missing")
    else:
        print(label, "present")


def local_attr_subscript_tuple():
    local = "local"
    box = Box()
    box.value = "attr"
    values = ["zero", "one", "two"]
    mapping = {"gone": "dict", "stay": "keep"}

    del local, box.value, values[1], mapping["gone"]

    show_missing("local", lambda: local)
    print("attr removed", hasattr(box, "value"))
    print("list after del", values)
    print("dict after del", mapping)


local_attr_subscript_tuple()


global_value = "global"


def delete_global():
    global global_value
    del global_value


delete_global()
show_missing("global", lambda: global_value)


class Watch:
    def __init__(self):
        self.first = "first"
        self.second = "second"

    def __delattr__(self, name):
        events.append("delattr:" + name)
        object.__delattr__(self, name)


class Index:
    def __init__(self, label, value):
        self.label = label
        self.value = value

    def __index__(self):
        events.append("index:" + self.label)
        return self.value


events = []
watch = Watch()
items = ["a", "b"]
try:
    del watch.first, items[Index("bad", 5)], watch.second
except IndexError:
    print("tuple failure", hasattr(watch, "first"), hasattr(watch, "second"), events)


root = Box()
root.child = Box()
root.child.value = "nested"
del root.child.value
print("chained attr", hasattr(root.child, "value"))

matrix = [["left", "middle", "right"]]
del matrix[0][1]
print("chained subscript", matrix)

targets = ["x", "y", "z"]
del (targets[0], targets[-1])
print("parenthesized targets", targets)
