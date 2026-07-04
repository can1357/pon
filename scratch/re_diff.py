import re, sys, json

pats = [
    r"(?:a|ab)*", r"(?:a|ab)*+", r"(?:ab|a)+", r"(?:ab|a)++", r"(?:a|ab)?",
    r"(a+)+", r"(a+)++", r"(a|aa)+", r"(a|aa)*+", r"[0-9]+(?:\.[0-9]+)*+",
    r"(?P<x>[0-9]+)?+(?P<y>[a-z]+)?+", r"a*?b", r"a+?b", r"(?:xy|x)*z",
    r"(?>(?:a|ab)*)", r"(?>a+)a", r"a{2,4}", r"a{2,4}+", r"a{2,4}?",
    r"(?:\d+|\w)+", r"(ab|a)*b", r"colou?r", r"(foo|foobar)$",
    r"\s*v?(\d+)\s*", r"(?:-(\d+))|(?:x)", r"(a?)*", r"(a*)*b",
]
strings = ["", "a", "ab", "aba", "abab", "aaa", "aaab", "12", "1.2.3", "12ab",
           "xyz", "color", "colour", "foobar", "  v42  ", "-99", "aaaa", "b", "xz", "xyxyz"]

def probe(kind, pat, s):
    try:
        rx = re.compile(pat)
    except Exception as e:
        return ("compile_err", type(e).__name__)
    try:
        m = getattr(rx, kind)(s)
    except Exception as e:
        return ("run_err", type(e).__name__)
    if m is None:
        return None
    return [m.group(0), m.groups(), m.span()]

out = {}
for pat in pats:
    for s in strings:
        for kind in ("match", "fullmatch", "search"):
            out[f"{kind}|{pat}|{s}"] = probe(kind, pat, s)
print(json.dumps(out, sort_keys=True))
