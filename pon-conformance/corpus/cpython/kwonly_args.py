def describe(name, *, prefix="item", suffix):
    return f"{prefix}:{name}:{suffix}"

print(describe("alpha", suffix="done"))
print(describe("beta", prefix="kind", suffix="ok"))
