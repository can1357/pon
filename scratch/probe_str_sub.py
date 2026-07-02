class S(str):
    pass
s = S("ab")
try:
    print(s.upper())
except Exception as e:
    print("ERR", type(e).__name__, e)
print(str.join)
