def f(s, **kwargs):
    print('before')
    print(s, **kwargs)
    print('after')

f('hello', flush=True)
