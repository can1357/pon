class B(bytes): pass
print(int(B(b'10')), int(B(b'10'), 16))
