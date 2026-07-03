# Derived from CPython v3.14.0 Lib/test/test_long.py topics (PSF license).
#
# `int.from_bytes` accepts CPython's `PyObject_Bytes` payload universe:
# bytes-like buffers (bytes/bytearray/memoryview), `__bytes__` carriers, and
# iterables of `__index__`-able ints in `range(0, 256)`; `byteorder` defaults
# to 'big' (3.11+) and `signed` re-reads the top bit.  `ipaddress` feeds
# `int.from_bytes(map(cls._parse_octet, octets), 'big')` at module exec.

# --- buffer payloads ----------------------------------------------------------
print(int.from_bytes(b'\x01\x00', 'big'))
print(int.from_bytes(b'\x01\x00'))
print(int.from_bytes(bytearray(b'\x01\x00'), 'big'))
print(int.from_bytes(memoryview(b'\x01\x00'), 'big'))
print(int.from_bytes(b'', 'big'))
print(int.from_bytes(b'\xfc\x00', 'big', signed=True))
print(int.from_bytes(b'\xfc\x00', 'big'))

# --- keyword forms ------------------------------------------------------------
print(int.from_bytes(bytes=b'\x01\x00', byteorder='big'))
print(int.from_bytes(b'\x01\x00', byteorder='little'))
print(int.from_bytes(b'\xff', byteorder='big', signed=True))

# --- iterable payloads --------------------------------------------------------
print(int.from_bytes([1, 0], 'big'))
print(int.from_bytes((255, 254), 'little'))
print(int.from_bytes(map(int, [169, 254, 0, 0]), 'big'))
print(int.from_bytes(iter([1, 2, 3]), 'big'))
print(int.from_bytes([], 'big'))
print(int.from_bytes([True, False], 'big'))

# --- dunder payloads ----------------------------------------------------------
class WithBytes:
    def __bytes__(self):
        return b'\x02\x01'

class WithIndex:
    def __index__(self):
        return 7

print(int.from_bytes(WithBytes(), 'big'))
print(int.from_bytes([WithIndex()], 'big'))
print(bytes([WithIndex()]))

# --- signed round-trips -------------------------------------------------------
for value in (0, 1, -1, 255, -256, 32767, -32768):
    packed = value.to_bytes(2, 'little', signed=True)
    print(value, int.from_bytes(packed, 'little', signed=True))

# --- error shapes -------------------------------------------------------------
def shape(fn):
    try:
        return fn()
    except Exception as exc:
        return f"{type(exc).__name__}: {exc}"

print(shape(lambda: int.from_bytes('161', 'big')))
print(shape(lambda: int.from_bytes(3, 'big')))
print(shape(lambda: int.from_bytes([256], 'big')))
print(shape(lambda: int.from_bytes([-1], 'big')))
print(shape(lambda: int.from_bytes([1.5], 'big')))
print(shape(lambda: int.from_bytes([1], 'sideways')))

# --- the ipaddress module-exec shape -------------------------------------------
import ipaddress
print(ipaddress.IPv4Address('169.254.0.0'))
print(int(ipaddress.IPv4Address('192.0.2.1')))
print(ipaddress.IPv4Network('169.254.0.0/16').num_addresses)
print(ipaddress.ip_address('2001:db8::1'))
print(int(ipaddress.IPv6Address('::ffff:c000:201')))
