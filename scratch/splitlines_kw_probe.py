s = 'a\nb\r\nc'
print(s.splitlines())
print(s.splitlines(True))
print(s.splitlines(keepends=True))
print(s.splitlines(keepends=False))
print(b'x\ny'.splitlines(keepends=True))
