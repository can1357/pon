def makers():
    funcs = []
    for value in range(3):
        def f(offset, value=value):
            return value + offset
        funcs.append(f)
    return funcs

print([func(10) for func in makers()])
