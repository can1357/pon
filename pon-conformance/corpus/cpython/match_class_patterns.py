class Point:
    __match_args__ = ("x", "y")

    def __init__(self, x, y):
        self.x = x
        self.y = y


def describe(p):
    match p:
        case Point(0, 0):
            return "origin"
        case Point(x=0, y=y):
            return f"y-axis:{y}"
        case Point(x, y) if x == y:
            return f"diag:{x}"
        case Point(x, y):
            return f"point:{x},{y}"
        case int(n):
            return f"int:{n}"
        case str() as s:
            return f"str:{s}"
        case _:
            return "other"


print(describe(Point(0, 0)))
print(describe(Point(0, 5)))
print(describe(Point(3, 3)))
print(describe(Point(1, 2)))
print(describe(42))
print(describe("hi"))
print(describe(None))
