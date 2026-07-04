import re

def fm(pat, s):
    m = re.fullmatch(pat, s)
    return None if m is None else m.group(0)

def mt(pat, s):
    m = re.match(pat, s)
    return None if m is None else m.group(0)

cases = [
    ("match (?:a|ab)* abab", lambda: mt(r"(?:a|ab)*", "abab")),
    ("fullmatch (?:a|ab)* abab", lambda: fm(r"(?:a|ab)*", "abab")),
    ("fullmatch (?:a|ab)*+ abab", lambda: fm(r"(?:a|ab)*+", "abab")),
    ("fullmatch (?>(?:a|ab)*) abab", lambda: fm(r"(?>(?:a|ab)*)", "abab")),
    ("match (?:a|ab)*+ abab", lambda: mt(r"(?:a|ab)*+", "abab")),
    ("match (?>(?:a|ab)*) abab", lambda: mt(r"(?>(?:a|ab)*)", "abab")),
    ("fullmatch (?:a|ab)++ ab", lambda: fm(r"(?:a|ab)++", "ab")),
    ("match (?:a|ab)++ ab", lambda: mt(r"(?:a|ab)++", "ab")),
    ("fullmatch (?:ab|a)++ ab", lambda: fm(r"(?:ab|a)++", "ab")),
    ("fullmatch (?:[0-9]+)?+ 12", lambda: fm(r"(?:[0-9]+)?+", "12")),
    ("fullmatch (?>(?:[0-9]+)?) 12", lambda: fm(r"(?>(?:[0-9]+)?)", "12")),
    ("match (?:[0-9]+)?+x 12x", lambda: mt(r"(?:[0-9]+)?+x", "12x")),
]
for label, fn in cases:
    print(f"{label!r} => {fn()!r}")
