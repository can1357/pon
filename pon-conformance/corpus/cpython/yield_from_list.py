def flatten():
    yield "start"
    yield from [1, 2, 3]
    yield "end"

print(list(flatten()))
