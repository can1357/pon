def mix(seed):
    acc = seed
    for i in range(32):
        acc = (acc * 1664525 + 1013904223 + i) % 65536
    return acc


def run():
    total = 0
    for i in range(1200):
        total = (total + mix(i)) % 1000003
    return total


print("int_loop")
print(run())
