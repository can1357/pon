count: int = 3
name: str

def annotated(value: int, label: "str") -> bool:
    return str(value) == label

print(annotated(3, "3"))
print(annotated.__annotations__)
print(__annotate__(1))
