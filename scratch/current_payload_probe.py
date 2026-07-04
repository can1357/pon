class S(str):
    pass
class I(int):
    pass
class B(bytes):
    pass
for C, arg in [(S, 'abc'), (I, 7), (B, b'abc')]:
    try:
        x = C(arg)
        print(C.__name__, repr(x), len(x) if not isinstance(x, int) else int(x), bool(x))
    except Exception as e:
        print(C.__name__, type(e).__name__, str(e))
