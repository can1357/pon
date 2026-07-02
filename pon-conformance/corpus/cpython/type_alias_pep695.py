type X = list[int]

print(X)
print(X.__name__)
print(X.__value__)

def identity[T](x: T) -> T:
    return x

print(identity(3))
print(identity.__annotations__)
