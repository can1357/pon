def combine(a, b=2, *, c=3, d):
    return a + b * 10 + c * 100 + d * 1000

print(combine(1, d=4))
print(combine(a=1, b=5, c=6, d=7))
