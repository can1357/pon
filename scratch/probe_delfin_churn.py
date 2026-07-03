import gc


deleted = False


class Finalized:
    def __del__(self):
        global deleted
        deleted = True


def churn():
    a = [1]
    b = [2]
    c = [3]
    d = [4]
    e = [5]
    f = [6]
    g = [7]
    h = [8]
    i = [9]
    j = [10]
    k = [11]
    m = [12]
    return a[0] + b[0] + c[0] + d[0] + e[0] + f[0] + g[0] + h[0] + i[0] + j[0] + k[0] + m[0]


obj = Finalized()
del obj
churn()
churn()
gc.collect()
gc.collect()
print("after churn", deleted)
