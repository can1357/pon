import gc

def f():
    a = [1]
    b = [2]
    c = [3]
    d = [4]
    e = [5]
    g = [6]
    h = [7]
    i = [8]
    j = [9]
    k = [10]
    l = [11]
    m = [12]
    gc.collect()
    return a[0] + b[0] + c[0] + d[0] + e[0] + g[0] + h[0] + i[0] + j[0] + k[0] + l[0] + m[0]

print(f())
