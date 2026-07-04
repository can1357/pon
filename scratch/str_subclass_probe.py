class S(str):
    pass

print(str(S('a')))
print('%s.%s' % (S('a'), 'b'))

class RaisesStr:
    def __str__(self):
        raise ValueError('inner-str')

try:
    str(RaisesStr())
except Exception as exc:
    print(type(exc).__name__, str(exc))
