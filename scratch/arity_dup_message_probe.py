def f(x): pass
try:
    f(1, x=2)
except TypeError as e:
    print(e)
try:
    f(**{'x':1}, x=2)
except TypeError as e:
    print(e)
