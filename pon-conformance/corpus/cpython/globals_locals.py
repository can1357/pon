g = globals()
print(globals() is g)
g["x"] = 1
print(x)
x = 2
print(g["x"])

def f(a):
    b = 3
    first = locals()
    first["a"] = 99
    b = 4
    second = locals()
    print(first["a"])
    print(a)
    print(first["b"])
    print(second["b"])

f(5)
