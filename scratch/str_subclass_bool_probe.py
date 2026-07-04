class S(str): pass
print(bool(S('')), bool(S('x')))
if S(''):
    print('truthy-branch')
else:
    print('falsy-branch')
print(bool(''), bool('x'))
class B(bytes): pass
print(bool(B(b'')), bool(B(b'x')))
class L(list): pass
print(bool(L()), bool(L([1])))
class T(tuple): pass
print(bool(T()), bool(T((1,))))
class D(dict): pass
print(bool(D()), bool(D(a=1)))
class I(int): pass
print(bool(I(0)), bool(I(5)))
