def f(): pass
for label, call in [
    ('star', lambda: f(*1)),
    ('dstar_nonmap', lambda: f(**1)),
    ('dstar_nonstr', lambda: f(**{1:2})),
]:
    try:
        call()
    except TypeError as e:
        print(label, e)
