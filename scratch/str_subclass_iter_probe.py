class S(str): pass
s = S('abc')
print(list(s), [c for c in s], tuple(iter(s)))
print(s[0], s[-1], s[1:], s[::-1], type(s[0]).__name__)
a, b, c = S('xyz')
print(a, b, c)
print(sorted(S('cba')))
