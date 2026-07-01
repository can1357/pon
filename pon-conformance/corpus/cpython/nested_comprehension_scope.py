x = "outer"
result = [(x, y) for x in range(2) for y in [x + 10]]
print(result)
print(x)
