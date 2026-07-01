def collect(a, b, c=0, d=0, **extra):
    print(a, b, c, d, sorted(extra.items()))

args1 = [1]
args2 = (2,)
kwargs1 = {"c": 3}
kwargs2 = {"d": 4, "e": 5}
collect(*args1, *args2, **kwargs1, **kwargs2)
