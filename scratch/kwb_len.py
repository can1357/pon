c = '\x8a'
print('len', len(c))
print('ord', ord(c))
print('eq', c == chr(138))
d = '\x7f'
print('len7f', len(d))
e = '\u00e9'
print('lene9', len(e))
class S:
    __slots__ = ('v',)
    def __init__(self, v): self.v = v
s = S(v=3)
print('isinst', isinstance(s, S))
