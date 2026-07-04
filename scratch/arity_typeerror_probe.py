def too_many(x):
    pass

def missing_pos(x):
    pass

def unexpected_kw(x):
    pass

def missing_kwonly(*, x):
    pass

cases = [
    ("too_many", lambda: too_many(1, 2)),
    ("missing_pos", lambda: missing_pos()),
    ("unexpected_kw", lambda: unexpected_kw(y=1)),
    ("missing_kwonly", lambda: missing_kwonly()),
]

for label, call in cases:
    try:
        call()
    except TypeError as e:
        print(label, e)
    else:
        print(label, "NO ERROR")
