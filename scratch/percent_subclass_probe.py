class S(str): pass
class T(tuple): pass
print('x=%s' % S('a'))
print('x=%s y=%s' % T(('a','b')))
print('%s' % (S('q'),))
print('%d' % 5, '%s %s' % ('a', S('b')))
class B(bytes): pass
print(b'%s' % B(b'z'))
d = {'k': S('v')}
print('%(k)s' % d)
