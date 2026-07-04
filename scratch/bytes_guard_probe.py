class I(int): pass
for expr in ["bytes(I(3))", "(I(1) in b'abc')"]:
    try:
        print(expr, '=>', eval(expr))
    except Exception as e:
        print(expr, type(e).__name__, str(e))
