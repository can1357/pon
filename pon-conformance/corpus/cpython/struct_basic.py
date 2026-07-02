# _struct surface: pack/unpack round-trips across formats and endianness,
# calcsize table, Struct class, pack_into/unpack_from/iter_unpack, and
# error legs (caught and printed by exception type name).
import struct

# calcsize across byte-order modes (native mode exercises C alignment).
for fmt in ['<2i3sxq', '>hhl', '=Bq', '!Hd', '@hi', '@ih', '@lqn', '<b', '10s', '3x']:
    print(fmt, struct.calcsize(fmt))

# Integer packing, both endiannesses, full signed/unsigned matrix.
print(struct.pack('<hHiIqQ', -2, 3, -4, 5, -6, 7))
print(struct.pack('>hHiIqQ', -2, 3, -4, 5, -6, 7))
print(struct.unpack('<hHiIqQ', struct.pack('<hHiIqQ', -32768, 65535, -2147483648, 4294967295, -(2**63), 2**64 - 1)))
print(struct.unpack('>hHiIqQ', struct.pack('>hHiIqQ', -32768, 65535, -2147483648, 4294967295, -(2**63), 2**64 - 1)))
print(struct.pack('<2b2B', -1, 2, 3, 255))
print(struct.unpack('>l', b'\xff\xff\xff\xfe'))
print(struct.pack('=H', 65535))

# Native mode round-trips (values only; sizes are host-dependent but the
# calcsize table above pins this platform's layout).
print(struct.unpack('@n', struct.pack('@n', -12345)))
print(struct.unpack('@N', struct.pack('@N', 12345)))
print(struct.unpack('@l', struct.pack('@l', -7)))

# Floats: exact binary fractions keep repr stable across implementations.
print(struct.pack('!d', 1.5), struct.unpack('!d', struct.pack('!d', 1.5)))
print(struct.pack('<f', -2.25), struct.unpack('<f', struct.pack('<f', -2.25)))
print(struct.unpack('<e', struct.pack('<e', 1.5)))
print(struct.unpack('>e', struct.pack('>e', -0.09375)))
print(struct.unpack('<d', struct.pack('<d', float('inf'))))
print(struct.pack('<d', 3), struct.unpack('<d', struct.pack('<d', 3)))

# Bytes-flavored codes: chars, padded strings, Pascal strings, bools.
print(struct.pack('3s', b'abcd'), struct.pack('6s', b'ab'), struct.pack('0s', b'x'))
print(struct.unpack('3s', b'abc'))
print(struct.pack('4p', b'abcdef'), struct.unpack('4p', struct.pack('4p', b'abcdef')))
print(struct.pack('?c?', True, b'z', False), struct.unpack('?c?', struct.pack('?c?', True, b'z', False)))
print(struct.unpack('2x3b', b'\x00\x00\x01\x02\x03'))

# The Struct class: attributes, methods, keyword offset.
s = struct.Struct('<ih')
print(s.format, s.size)
print(s.pack(7, -8), s.unpack(s.pack(7, -8)))
print(s.unpack_from(b'\x00\x00\x00\x00' + s.pack(9, 10), 4))
print(s.unpack_from(b'\x00\x00\x00\x00' + s.pack(9, 10), offset=4))
print(struct.unpack_from('<h', b'\x01\x00\x02\x00', -2))
print(struct.unpack_from('<h', bytearray(b'\x05\x00')))
print(struct.unpack('<h', memoryview(b'\x06\x00')))

# In-place packing into a mutable buffer.
buf = bytearray(10)
struct.pack_into('<i', buf, 2, 123456)
print(bytes(buf))
s.pack_into(buf, 0, -1, -2)
print(bytes(buf))

# Iterated unpacking.
print(list(struct.iter_unpack('<h', b'\x01\x00\x02\x00\x03\x00')))
print(list(struct.iter_unpack('>2b', bytes(range(6)))))
for pair in struct.iter_unpack('<bB', b'\x01\x02\x03\x04'):
    print(pair)

# Error legs: wrong sizes, bad format chars, range and type failures.
legs = [
    lambda: struct.pack('<i', 2**31),
    lambda: struct.pack('B', 256),
    lambda: struct.pack('b', -129),
    lambda: struct.pack('h', 40000),
    lambda: struct.pack('<i', 'x'),
    lambda: struct.pack('<z', 1),
    lambda: struct.pack('<i'),
    lambda: struct.pack('<i', 1, 2),
    lambda: struct.pack('4s', 'nope'),
    lambda: struct.pack('c', b'ab'),
    lambda: struct.unpack('<i', b'\x00'),
    lambda: struct.unpack('<h', b'\x00\x00\x00'),
    lambda: struct.calcsize('3'),
    lambda: struct.calcsize('<n'),
    lambda: struct.calcsize('=P'),
    lambda: struct.unpack_from('<i', b'\x00\x00', 0),
    lambda: struct.unpack_from('<i', b'\x00\x00\x00\x00', 12),
    lambda: list(struct.iter_unpack('<h', b'\x01\x00\x02')),
    lambda: list(struct.iter_unpack('', b'')),
    lambda: struct.pack_into('<i', bytearray(2), 0, 1),
    lambda: struct.Struct(3),
]
for leg in legs:
    try:
        leg()
        print('no error')
    except struct.error:
        print('caught struct.error')
    except Exception as exc:
        print('caught other', type(exc).__name__)

# The exception class itself is a real Exception subclass living in 'struct'.
print(struct.error.__name__, struct.error.__module__)
print(issubclass(struct.error, Exception))
try:
    raise struct.error('manual raise')
except Exception as exc:
    print('re-raised', type(exc).__name__)

struct._clearcache()
print('done')
