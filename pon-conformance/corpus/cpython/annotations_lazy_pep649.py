def broken(x: Undefined) -> None:
    return None

print("defined ok")

try:
    broken.__annotations__
except NameError as e:
    print(e)

def annotated(value: int) -> str:
    return str(value)

print(annotated.__annotations__ is annotated.__annotations__)
print(annotated.__annotations__)

class Node:
    parent: "Node"
    count: int = 0

print(Node.__annotations__)
print(Node.count)
