def classify(v):
    match v:
        case 0 | 1 | 2:
            return "small"
        case [x] | (x,):
            return f"single:{x}"
        case {"a": a} | {"b": a}:
            return f"ab:{a}"
        case x if x is None:
            return "none"
        case _:
            return "big"


print(classify(1))
print(classify([9]))
print(classify((4,)))
print(classify({"b": 5}))
print(classify({"a": 2}))
print(classify(None))
print(classify(99))
print(classify(0))
