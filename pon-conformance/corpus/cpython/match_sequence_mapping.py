def seq(v):
    match v:
        case []:
            return "empty"
        case [x]:
            return f"one:{x}"
        case [first, *mid, last] if mid:
            return f"wide:{first}:{len(mid)}:{last}"
        case [a, b]:
            return f"pair:{a},{b}"
        case _:
            return "no"


print(seq([]))
print(seq([7]))
print(seq([1, 2]))
print(seq([1, 2, 3, 4]))
print(seq((5, 6)))
print(seq("ab"))
print(seq({"a": 1}))


def mapping(v):
    match v:
        case {"cmd": "go", "n": n, **rest}:
            return f"go:{n}:{len(rest)}"
        case {"cmd": c}:
            return f"cmd:{c}"
        case {}:
            return "anymap"
        case _:
            return "no"


print(mapping({"cmd": "go", "n": 3, "extra": 1, "more": 2}))
print(mapping({"cmd": "stop", "n": 9}))
print(mapping({"other": 1}))
print(mapping([1, 2]))
