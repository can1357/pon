class B(bytes): pass
print(hash(B(b'abc')) == hash(b'abc'))
print({B(b'abc'): 'ok'}[b'abc'])
