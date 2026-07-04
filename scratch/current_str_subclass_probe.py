class S(str): pass
s=S('abc')
for expr in ['list(s)', 's[0]', 's[1:]', "'b' in s"]:
    try:
        print(expr, '=>', eval(expr))
    except Exception as e:
        print(expr, 'ERR', type(e).__name__, str(e))
