def mix(seed):
    acc = seed
    for i in range(4):
        acc = (acc * 3 + 1 + i) % 7
    return acc
print(mix(1))
