class B(bytes): pass
b=B(b'abc')
print(repr(b))
print(f'{b!r}')
print(str(b))
