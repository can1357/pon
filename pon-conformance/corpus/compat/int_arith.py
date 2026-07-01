def factorial(n):
    result = 1
    for i in range(1, n + 1):
        result = result * i
    return result

print(factorial(10))
print(factorial(20))
print(2 ** 10)
print(17 // 5)
print(17 % 5)
print(1 << 20)
print(255 & 15)
print(sum(range(100)))
total = 0
for i in range(10):
    total = total + i * i
print(total)
