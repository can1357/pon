# Derived from CPython v3.14.0 Lib/test/test_augassign.py topics (PSF license).

class Box:
    def __init__(self, value):
        self.value = value


class Shelf:
    def __init__(self):
        self.counts = {"left": 2, "right": 5}
        self.primary = Box(3)


box = Box(2)
box.value += 3
box.value *= 2
box.value //= 5
box.value %= 3
print(box.value)

shelf = Shelf()
shelf.primary.value += 4
shelf.primary.value *= 3
print(shelf.primary.value)

shelf.counts["left"] += 7
shelf.counts["right"] *= 2
print(shelf.counts["left"])
print(shelf.counts["right"])

alias = shelf.primary
shelf.primary.value -= 6
shelf.primary.value //= 3
print(alias.value)
print(alias is shelf.primary)
