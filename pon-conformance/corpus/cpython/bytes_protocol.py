payload = b'abc'

it = iter(payload)
print(type(it).__name__, it is iter(it))
print(next(it), next(it), next(it))
try:
    next(it)
except StopIteration:
    print("exhausted")
print(list(iter(b'')), list(b'\x00\xff'), [x for x in b'AB'])
print(type(iter(b'')) is type(iter(b'xyz')))
total = 0
for byte in "hi".encode():
    total += byte
print(total)

print(b'a' == b'a', b'a' == b'b', b'a' != b'b', b'a' != b'a')
print(b'a' < b'b', b'a' <= b'a', b'b' > b'a', b'b' >= b'c', b'ab' < b'b', b'a' < b'ab')
print(b'' == b'', b'' < b'a', b'abc' == b'abc', b'abd' > b'abc')
print(b'a' == 'a', b'a' != 'a', b'a' == 97, b'' == None)
print(b'a' == bytearray(b'a'), bytearray(b'a') == b'a', bytearray(b'a') < b'b', b'b' > bytearray(b'a'))
try:
    print(b'a' < 'b')
except TypeError as exc:
    print("TypeError", exc)
print(sorted([b'b', b'', b'ab', b'a']), min(b'cab'), max(b'cab'))

print(97 in b'ab', 99 in b'ab', 0 in b'\x00', 255 in b'\xff', 0 in b'ab')
print(b'a' in b'ab', b'ab' in b'ab', b'ba' in b'ab', b'' in b'ab', b'' in b'', b'abc' in b'ab')
print(98 not in b'ab', b'q' not in b'ab')
try:
    print(300 in b'ab')
except ValueError as exc:
    print("ValueError", exc)
try:
    print(-1 in b'ab')
except ValueError as exc:
    print("ValueError", exc)
try:
    print('a' in b'ab')
except TypeError as exc:
    print("TypeError", exc)

print(b'')
print(b'hello')
print(repr(b'ab'), str(b'ab'))
print(b"it's")
print(b'say "hi"')
print(b'mixed\'and"quotes')
print(b'a\tb\nc\rd\\e')
print(b'\x00\x7f\x80\xff')
print(repr(''.join(chr(i) for i in range(32, 40)).encode()))

print(bytes(), bytes(0), bytes(3))
print(bytes([65, 66, 67]), bytes(range(4)), bytes((255,)))
print(bytes(b'copy'), bytes(bytearray(b'from-ba')))
print(bytes('hé', 'utf-8'), bytes('hi', 'ascii', 'strict'), bytes('h\xff', 'latin-1'))
print(bytes(True), bytes(x * 2 for x in [1, 2, 3]))
try:
    bytes('x')
except TypeError as exc:
    print("TypeError", exc)
try:
    bytes(-1)
except ValueError as exc:
    print("ValueError", exc)
try:
    bytes([300])
except ValueError as exc:
    print("ValueError", exc)
try:
    bytes([1.5])
except TypeError as exc:
    print("TypeError", exc)
try:
    bytes(1.5)
except TypeError as exc:
    print("TypeError", exc)

print(hash(b'a') == hash(b'a'), hash(b'') == hash(b''), hash(b'a') == hash(b'ab'))
table = {b'k1': 1, b'k2': 2}
table[b'k1'] = 10
print(table[b'k1'], table[b'k2'], b'k1' in table, b'k3' in table, len(table))
print(table)
print(len({b'x', b'x', b'y'}), sorted({b'b', b'a', b'b'}))

seen = {}
for chunk in [b'aa', b'bb', b'aa']:
    seen[chunk] = seen.get(chunk, 0) + 1
print(sorted(seen.items()))

ba = bytearray(b'xyz')
ba_iter = iter(ba)
print(type(ba_iter).__name__, next(ba_iter), list(ba_iter), list(bytearray()))
print(type(iter(bytearray())) is type(iter(bytearray(b'q'))))
print(bytearray(b'a') == bytearray(b'a'), bytearray(b'a') != bytearray(b'b'), bytearray(b'a') < bytearray(b'b'), bytearray(b'b') >= bytearray(b'a'))
print(120 in ba, 113 in ba, b'yz' in ba, bytearray(b'xy') in b'xyz')
grow = bytearray(b'ab')
grown = []
for byte in grow:
    grown.append(byte)
    if len(grown) == 1:
        grow.append(99)
print(grown, grow)

table = bytes(range(255, -1, -1))
print(b'\x00\x01\xff'.translate(table), bytearray(b'\x00\xff').translate(table))
print(b'0000'.translate(b'0' + b'1' * 255), b'abcabc'.translate(None, b'b'))
print(b'abc'.translate(None), b'spam and eggs'.translate(bytes(range(256)), b'aeiou'))
bits = bytearray(8)
bits[1] = 1
bits[6] = 1
s = bits.translate(b'0' + b'1' * 255)[::-1]
print(s, int(s, 2), int(b'0101', 2), int(b'ff', 16), int(bytearray(b'42')), int(b' 7 '))
print(b'abcb'.find(98), b'abcb'.rfind(98), b'abcb'.index(99), b'abcb'.count(98), b'abcb'.find(120), bytearray(b'abcb').find(98, 2))
print(b'abcd'[::-1], bytearray(b'abcd')[::-1], b'abcdef'[4:1:-2])
try:
    b'ab'[9]
except IndexError as exc:
    print("IndexError", exc)
try:
    bytearray(b'ab')[9] = 1
except IndexError as exc:
    print("IndexError", exc)
charmap = bytearray(4)
try:
    charmap[300] = 1
except IndexError:
    charmap += b'\x00' * 297
    charmap[300] = 1
print(len(charmap), charmap[300], charmap.find(1), charmap.find(1, 301), charmap.count(0))
