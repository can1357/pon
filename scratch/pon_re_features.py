import re

def show(label, fn):
    try:
        print(label, "=>", fn())
    except Exception as e:
        print(label, "ERR", type(e).__name__, e)

# possessive quantifiers
show("a*+ vs aaa", lambda: bool(re.compile(r"a*+").fullmatch("aaa")))
show("a++ vs aaa", lambda: bool(re.compile(r"a++").fullmatch("aaa")))
show("a?+ vs a", lambda: bool(re.compile(r"a?+").fullmatch("a")))
show("(?:ab)*+ vs abab", lambda: bool(re.compile(r"(?:ab)*+").fullmatch("abab")))
# atomic group
show("atomic (?>a+)b vs aaab", lambda: bool(re.compile(r"(?>a+)b").fullmatch("aaab")))
# scoped inline flag group
show("(?a:\\d+) vs 12", lambda: bool(re.compile(r"(?a:\d+)").fullmatch("12")))
# named + possessive combo
show("(?P<x>[0-9]+)?+ vs 12", lambda: bool(re.compile(r"(?P<x>[0-9]+)?+").fullmatch("12")))
# the dev part
show("dev part", lambda: bool(re.compile(r"(?P<dev>[._-]?+(?P<dev_l>dev)[._-]?+(?P<dev_n>[0-9]+)?)?+").fullmatch("dev0")))
show("release possessive", lambda: bool(re.compile(r"(?P<release>[0-9]+(?:\.[0-9]+)*+)").fullmatch("0")))
