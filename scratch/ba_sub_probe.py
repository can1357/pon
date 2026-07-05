class MyByteArray(bytearray):
    pass
x = MyByteArray(b"mut")
for expr in ["type(x).__name__", "repr(x)", "len(x)", "bytes(x)", "bytearray(x)", "x[0]", "isinstance(x, bytearray)"]:
    try:
        print(expr, "=>", eval(expr))
    except BaseException as exc:
        print(expr, "ERR", type(exc).__name__, str(exc))
