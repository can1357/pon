import gc

def f():
    x = [1, 2, 3]
    y = {"a": 1}
    s = "abc" * 7
    gc.collect()
    gc.collect()
    return x[0] + x[1] + x[2] + y["a"], s[:3]

print(f())
