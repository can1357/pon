class S(str): pass
s = S('hello world')
print(len(s), bool(S('')), 'lo' in s, 'zz' in s)
try:
    3 in s
except TypeError as e:
    print('TE', e)
print('world' in S('hello world'), S('') in s)
