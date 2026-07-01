def counter(start):
    value = start
    def inc(step):
        nonlocal value
        value += step
        return value
    return inc

next_value = counter(10)
print(next_value(1))
print(next_value(5))
