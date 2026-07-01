def fib(n):
    a = 0
    b = 1
    for i in range(n):
        next_value = a + b
        a = b
        b = next_value
    return a


def run():
    total = 0
    for i in range(40):
        total = total + fib(20 + (i % 5))
    return total


print("fib")
print(run())
