import re
def fm(pat, s):
    m = re.fullmatch(pat, s)
    return None if m is None else m.group(0)
cases = [
    (r"(?:)+", ""),
    (r"(?:a*)+", ""),
    (r"(?:a*)+", "aaa"),
    (r"(?:a*)*+", "aaa"),
    (r"(?:a*)++", "aaa"),
    (r"(?:x?)+", ""),
    (r"(?:x?)+y", "y"),
    (r"(?=a)*", ""),
    (r"(?:b*)*", "bbb"),
]
for pat, s in cases:
    print(f"{pat!r} on {s!r} => {fm(pat, s)!r}")
