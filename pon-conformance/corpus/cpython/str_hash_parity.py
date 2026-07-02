# CPython seed-0 str/bytes hash parity (the runner pins PYTHONHASHSEED=0):
# raw hash() values across the PEP 393 widths, plus dict-key round-trips.
print(hash(''))
print(hash(b''))
print(hash('a'))
print(hash('abc'))
print(hash('abcé'))
print(hash('\u0394'))
print(hash('a\u0394'))
print(hash('\U0001F600'))
print(hash('x' * 64))
print(hash(b'bytes'))
print(hash(b'abc'))
print(hash('a') == hash(b'a'))
print(hash('abcé') == hash('abcé'.encode('utf-8')))
print(hash('né'))

d = {'a': 1, 'abcé': 2, b'bytes': 3, 'x' * 64: 4}
d['a'] += 10
d['\u0394'] = 5
print(d['a'], d['abcé'], d[b'bytes'], d['x' * 64], d['\u0394'])
print('a' in d, 'abcé' in d, b'bytes' in d, 'missing' in d, b'a' in d)
print(sorted(k if isinstance(k, str) else k.decode() for k in d))
lookup = 'ab' + 'cé'
print(d[lookup])
s = {'a', 'b', 'a', 'abcé'}
print(len(s), 'a' in s, 'abcé' in s, 'c' in s)
