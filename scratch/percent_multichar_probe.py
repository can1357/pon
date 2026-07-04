class S(str): pass
print('x=%s' % S('abc'))
print('%r' % S('ab'))
class T(tuple): pass
print('%s' % T(('only',)))
print('100%%' % ())
