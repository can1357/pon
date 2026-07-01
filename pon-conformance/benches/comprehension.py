def run():
    total = 0
    for outer in range(400):
        values = [(x * y + outer) % 97 for x in range(30) if x % 3 != 1 for y in range(5) if y != 2]
        total = total + len(values)
        for item in values:
            total = (total + item) % 1000003
    return total


print("comprehension")
print(run())
