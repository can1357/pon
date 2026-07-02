STEP = 3
LIMIT = 200000


def run():
    total = 0
    i = 0
    while i < LIMIT:
        total = total + STEP
        i = i + 1
    return total


print(run())
