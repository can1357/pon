def squares(limit):
    current = 0
    while current < limit:
        yield current * current
        current = current + 1


def shifted(limit):
    for value in squares(limit):
        yield value + 1


def run():
    total = 0
    for outer in range(800):
        for value in shifted(24):
            total = (total + value + outer) % 1000003
    return total


print("generator")
print(run())
