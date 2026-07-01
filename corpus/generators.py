def gen():
    yield 1
    yield 2

for value in gen():
    print(value)
