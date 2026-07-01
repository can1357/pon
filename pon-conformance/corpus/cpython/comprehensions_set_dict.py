squares = {x * x for x in range(6) if x % 2}
lookup = {chr(65 + x): x * x for x in range(4)}
print(sorted(squares))
print(list(lookup.items()))
