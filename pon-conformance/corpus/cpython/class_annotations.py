class Box:
    value: int
    label: str = "box"

    def __init__(self, value: int):
        self.value = value

print(Box.__annotations__)
print(Box(5).value, Box.label)
