def describe(value):
    match value:
        case {"kind": "point", "x": x, "y": y}:
            return f"point:{x},{y}"
        case [first, second, *rest] if rest:
            return f"seq:{first}:{second}:{len(rest)}"
        case _:
            return "other"

print(describe({"kind": "point", "x": 2, "y": 5}))
print(describe([1, 2, 3, 4]))
print(describe(None))
