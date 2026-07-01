def inner():
    yield 2
    yield 4
    return 6

def outer():
    result = yield from inner()
    yield result

print(list(outer()))
