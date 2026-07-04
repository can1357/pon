def f(x, /, y): pass
for src, call in [
    ('posonly_one', lambda: f(x=1, y=2)),
    ('missing_two', lambda: f()),
    ('missing_kwonly_two', lambda: (lambda *, x, y: None)()),
    ('too_many_zero', lambda: (lambda: None)(1)),
]:
    try:
        call()
    except TypeError as e:
        print(src, e)
