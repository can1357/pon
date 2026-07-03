# CPython's one-argument `str.maketrans(dict)` form: length-1 str keys
# re-key to their ordinal, int keys pass through, values are unvalidated,
# and the result is a real dict `str.translate` consumes.  `_pyrepl.utils`
# builds `ZERO_WIDTH_TRANS` this way at import on the doctest -> pdb chain.
t = str.maketrans({"\x01": "", "\x02": "", "a": "X", 98: None, "c": 120})
print(sorted(t.items(), key=lambda kv: kv[0]))
print("abc".translate(t))
print("\x01hi\x02".translate(str.maketrans({"\x01": "", "\x02": ""})))
try:
    str.maketrans({"ab": "X"})
except ValueError as e:
    print("VE:", e)
try:
    str.maketrans({b"a": "X"})
except TypeError as e:
    print("TE:", e)
try:
    str.maketrans([1, 2])
except TypeError as e:
    print("TE2:", e)
print(str.maketrans({}) == {})
print(str.maketrans({True: "x"})[1])
