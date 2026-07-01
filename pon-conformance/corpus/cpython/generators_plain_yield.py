def numbers(limit):
    current = 0
    while current < limit:
        yield current * current
        current += 1

it = numbers(4)
print(next(it))
print(list(it))
