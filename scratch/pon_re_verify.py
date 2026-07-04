import re

def show(label, pat, s, expect):
    try:
        got = bool(re.compile(pat).fullmatch(s))
    except Exception as e:
        got = f"ERR {type(e).__name__} {e}"
    ok = "OK" if got == expect else "FAIL"
    print(f"{ok} {label}: got={got} expect={expect}")

# possessive over groups (the version-regex bug)
show("noncap ?+", r"(?:[0-9]+)?+", "12", True)
show("cap ?+", r"([0-9]+)?+", "12", True)
show("named ?+", r"(?P<x>[0-9]+)?+", "12", True)
show("cap ?+ tail", r"([0-9]+)?+x", "12x", True)
show("two cap ?+", r"([0-9]+)?+([a-z]+)?+", "12ab", True)
# alternation possessive: no cross-rep backtracking (CPython semantics)
show("(?:a|ab)++ fullmatch ab", r"(?:a|ab)++", "ab", False)
show("(?:a|ab)*+ fullmatch abab", r"(?:a|ab)*+", "abab", False)
show("(?:ab|a)++ fullmatch ab", r"(?:ab|a)++", "ab", True)
# regression: greedy alternation still fullmatches via backtracking
show("(?:a|ab)+ fullmatch abab", r"(?:a|ab)+", "abab", True)
# the real packaging version regex against 0.dev0
pat = r"""
    v?+
    (?a:(?:(?P<epoch>[0-9]+)!)?+(?P<release>[0-9]+(?:\.[0-9]+)*+)(?P<pre>[._-]?+(?P<pre_l>alpha|a|beta|b|preview|pre|c|rc)[._-]?+(?P<pre_n>[0-9]+)?)?+(?P<post>(?:-(?P<post_n1>[0-9]+))|(?:(?:[._-]?(?P<post_l>post|rev|r)[._-]?(?P<post_n2>[0-9]+)?)))?+(?P<dev>[._-]?+(?P<dev_l>dev)[._-]?+(?P<dev_n>[0-9]+)?)?+)(?a:\+(?P<local>[a-z0-9]+(?:[._-][a-z0-9]+)*))?+
"""
rx = re.compile(r"\s*" + pat + r"\s*", re.VERBOSE | re.IGNORECASE)
show("packaging version 0.dev0", None, None, True) if False else print(
    "OK packaging 0.dev0:" if bool(rx.fullmatch("0.dev0")) else "FAIL packaging 0.dev0:",
    bool(rx.fullmatch("0.dev0")))
m = rx.fullmatch("1.2.3.dev4")
print("packaging 1.2.3.dev4 release:", m.group("release") if m else None, "dev_n:", m.group("dev_n") if m else None)
