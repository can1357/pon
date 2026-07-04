class S(str): pass
print(bool(S('')), bool(S('x')), len(S('abc')), len(S('')))
if S(''):
    print('truthy-branch')
else:
    print('falsy-branch')
print(bool(''), bool('x'))
class I(int): pass
print(bool(I(0)), bool(I(5)))
