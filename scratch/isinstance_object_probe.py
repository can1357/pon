for x in [{}.get, len, [].append, iter([]), staticmethod(len), (i for i in [1])]:
    print(type(x).__name__, isinstance(x, object))
class C: pass
print('C', isinstance(C(), object), isinstance(1, object), isinstance('s', object), isinstance(None, object))
print('issub', issubclass(type({}.get), object))
