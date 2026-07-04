def fmt_set(value):
    return type(value).__name__ + ":" + ",".join(sorted(repr(item) for item in value))


def show(label, value):
    print(label, repr(value))


def show_set(label, value):
    print(label, fmt_set(value))


def show_error(label, thunk):
    try:
        value = thunk()
    except Exception as exc:
        print(label, type(exc).__name__, str(exc))
    else:
        print(label, "NOERROR", repr(value))


names = {'a': 1, 'b': 2}
show("view type names", (type(names.keys()).__name__, type(names.values()).__name__, type(names.items()).__name__))
show("reprs", (repr(names.keys()), repr(names.values()), repr(names.items())))
show("len values", len(names.values()))
show("contains keys/items", ('a' in names.keys(), ('a', 1) in names.items(), 1 in names.values()))

view = names.keys()
show("iterate twice", (list(view), list(view)))
names['c'] = 3
show("mutation visibility", list(view))

show_set("keys diff set", {'a': 1}.keys() - {'a'})
show_set("keys intersect set", {'a': 1, 'b': 2}.keys() & {'b', 'c'})
show_set("items union set", {'a': 1}.items() | {('b', 2)})
show_set("reversed set diff", {'a', 'c'} - {'a': 1}.keys())

both = {'a': 1, 'b': 2}
show_set("keys diff iterable", both.keys() - ['a'])
show_set("iterable diff keys", ['a', 'c'] - both.keys())
show_set("keys and iterable", both.keys() & ['b', 'c'])
show_set("iterable and keys", ['b', 'c'] & both.keys())
show_set("keys or iterable", both.keys() | ['c'])
show_set("iterable or keys", ['c'] | both.keys())
show_set("keys xor iterable", both.keys() ^ ['b', 'c'])
show_set("iterable xor keys", ['b', 'c'] ^ both.keys())
show_set("items reflected xor", [('a', 1), ('c', 3)] ^ {'a': 1}.items())

show("set equality", {'a': 1}.keys() == {'a'})
show("isdisjoint", ({'a': 1}.keys().isdisjoint({'z'}), {'a': 1}.items().isdisjoint([('a', 1)])))
show_error("values set op", lambda: {'a': 1}.values() - {1})
