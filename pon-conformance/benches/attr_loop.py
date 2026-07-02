class Point:
    def __init__(self, x, y):
        self.x = x
        self.y = y


def run(p, n):
    total = 0
    i = 0
    while i < n:
        total = total + p.x + p.y
        i = i + 1
    return total


print(run(Point(3, 4), 200000))
