left = {1, 2, 3}
right = {3, 4}
print(sorted(left | right))
print(sorted(left & right))
print(sorted(left - right))
left.add(5)
left.discard(2)
print(sorted(left))
print(3 in left, 2 in left)
