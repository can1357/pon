pairs = [(x, y, x * y) for x in range(4) if x % 2 == 0 for y in range(3) if y != 1]
print(pairs)
print([value for row in [[1, 2], [3], [], [4, 5]] for value in row if value > 2])
