empty = b''
print(len(empty))
abc = b'abc'
print(abc[0], abc[1], abc[2], len(abc))
esc = b'\t\n\r\\\'\"'
print(esc[0], esc[1], esc[2], esc[3], esc[4], esc[5], len(esc))
hexed = b'\x00\x7f\xff'
print(hexed[0], hexed[1], hexed[2], hexed.hex())
print(b'hello world'.decode('utf-8'))
print(b'\xc3\xa9'.decode('utf-8'))
print(b'Gr\xc3\xbc\xc3\x9fe'.decode('utf-8'))
long = b'0123456789' * 20
print(len(long), long[0], long[199])
joined = b'abc' + b'' + b'def'
print(len(joined), joined[3], joined.decode())
print(b'AbC'.upper().decode(), b'AbC'.lower().decode())
print(b'a b c'.split()[1].decode())
print(b'unittest'.startswith(b'unit'), b'unittest'.endswith(b'test'))
print(b'banana'.find(b'na'), b'banana'.count(b'na'))
print((b'implicit' b'concat').decode())
