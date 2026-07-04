import re

def fm(pat, s):
    m = re.fullmatch(pat, s)
    return None if m is None else m.group(0)

def mt(pat, s):
    m = re.match(pat, s)
    return None if m is None else m.group(0)

cases = [
    ("match (?:a|ab)* abab", lambda: mt(r"(?:a|ab)*", "abab"), "a"),
    ("fullmatch (?:a|ab)* abab", lambda: fm(r"(?:a|ab)*", "abab"), "abab"),
    ("fullmatch (?:a|ab)*+ abab", lambda: fm(r"(?:a|ab)*+", "abab"), None),
    ("fullmatch (?>(?:a|ab)*) abab", lambda: fm(r"(?>(?:a|ab)*)", "abab"), None),
    ("match (?:a|ab)*+ abab", lambda: mt(r"(?:a|ab)*+", "abab"), "a"),
    ("match (?>(?:a|ab)*) abab", lambda: mt(r"(?>(?:a|ab)*)", "abab"), "a"),
    ("fullmatch (?:a|ab)++ ab", lambda: fm(r"(?:a|ab)++", "ab"), None),
    ("match (?:a|ab)++ ab", lambda: mt(r"(?:a|ab)++", "ab"), "a"),
    ("fullmatch (?:ab|a)++ ab", lambda: fm(r"(?:ab|a)++", "ab"), "ab"),
    ("fullmatch (?:[0-9]+)?+ 12", lambda: fm(r"(?:[0-9]+)?+", "12"), "12"),
    ("fullmatch (?>(?:[0-9]+)?) 12", lambda: fm(r"(?>(?:[0-9]+)?)", "12"), "12"),
]
for label, fn, expect in cases:
    got = fn()
    print(("OK " if got == expect else "DIFF ") + f"{label}: got={got!r} cpython={expect!r}")
