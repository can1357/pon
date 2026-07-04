import re

def show(label, pat, s):
    try:
        print(label, "=>", bool(re.compile(pat).fullmatch(s)))
    except Exception as e:
        print(label, "ERR", type(e).__name__, e)

show("noncap ?+ group", r"(?:[0-9]+)?+", "12")
show("cap ?+ group", r"([0-9]+)?+", "12")
show("named ?+ group", r"(?P<x>[0-9]+)?+", "12")
show("cap *+ group", r"([0-9]+)*+", "12")
show("cap ++ group", r"([0-9]+)++", "12")
show("cap ?+ then tail", r"([0-9]+)?+x", "12x")
show("cap greedy ? group", r"([0-9]+)?", "12")
show("two cap ?+", r"([0-9]+)?+([a-z]+)?+", "12ab")
