def show(label, fn):
    try: print(label, "=>", fn())
    except Exception as e: print(label, "ERR", type(e).__name__, e)
show("set.isdisjoint", lambda: {1,2}.isdisjoint({3}))
show("frozenset.isdisjoint", lambda: frozenset({1}).isdisjoint({2}))
show("dict_keys.isdisjoint", lambda: {1:0}.keys().isdisjoint({2}))
show("dict_items.isdisjoint", lambda: {1:0}.items().isdisjoint({(2,0)}))
import tomllib
show("tomllib.loads", lambda: tomllib.loads('a = 1\nb = [1,2]\n'))
