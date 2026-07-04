for expr in ["object.__init_subclass__(total=False)", "dict.__init_subclass__(total=False)", "tuple.__init_subclass__()"]:
    try:
        exec(expr)
    except Exception as exc:
        print(expr, type(exc).__name__, exc)
