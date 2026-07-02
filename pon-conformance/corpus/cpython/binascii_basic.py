import binascii

# --- module surface -----------------------------------------------------------
print(issubclass(binascii.Error, ValueError), issubclass(binascii.Error, Exception))
print(issubclass(binascii.Incomplete, Exception))
for name in ["a2b_base64", "b2a_base64", "a2b_hex", "b2a_hex", "hexlify",
             "unhexlify", "a2b_qp", "b2a_qp", "a2b_uu", "b2a_uu", "crc32", "crc_hqx"]:
    print(name, callable(getattr(binascii, name)))

# --- hex round-trips ------------------------------------------------------------
raw = bytes(range(0, 256, 7)) + b"\x00\xff"
h = binascii.hexlify(raw)
print(h)
print(binascii.unhexlify(h) == raw)
print(binascii.b2a_hex(b"pon"), binascii.a2b_hex(b"706f6e"))
print(binascii.a2b_hex("706F6E"), binascii.unhexlify(bytearray(b"01ff")))
print(binascii.hexlify(b""), binascii.unhexlify(b""))
print(binascii.hexlify(b"\x01\x02\x03\x04\x05", b"_"))
print(binascii.hexlify(b"\x01\x02\x03\x04\x05", "_", 2))
print(binascii.hexlify(b"\x01\x02\x03\x04\x05", b":", -2))
print(binascii.hexlify(b"\x01\x02", b":", 4))

# --- hex error legs --------------------------------------------------------------
try:
    binascii.unhexlify(b"abc")
except binascii.Error as exc:
    print("Error:", exc)
try:
    binascii.unhexlify(b"zz")
except binascii.Error as exc:
    print("Error:", exc)
try:
    binascii.hexlify(b"ab", b"__")
except ValueError as exc:
    print("ValueError:", exc)

# --- base64 round-trips -----------------------------------------------------------
print(binascii.b2a_base64(b""))
print(binascii.b2a_base64(b"f"), binascii.b2a_base64(b"fo"), binascii.b2a_base64(b"foo"))
print(binascii.b2a_base64(b"foobar", newline=False))
big = bytes(range(256)) * 3
encoded = binascii.b2a_base64(big)
print(len(encoded), binascii.a2b_base64(encoded) == big)
print(binascii.a2b_base64(b"Zm9vYmFy"))
print(binascii.a2b_base64("Zm9vYmFy"))
print(binascii.a2b_base64(b"Zm\n9v"), binascii.a2b_base64(b"Zm$$9v"))
print(binascii.a2b_base64(b"Zg==Zg=="), binascii.a2b_base64(b"AB==CD=="))
print(binascii.a2b_base64(b"Zg=,="), binascii.a2b_base64(b"Zm9v===="), binascii.a2b_base64(b"="))
try:
    binascii.a2b_base64(b"Zg==trailing-junk")
except binascii.Error as exc:
    print("Error:", exc)
print(binascii.a2b_base64(b"Zm9vYmFy", strict_mode=True))
print(binascii.a2b_base64(b""), binascii.a2b_base64(b"", strict_mode=True))

# --- base64 error legs --------------------------------------------------------------
try:
    binascii.a2b_base64(b"Zg=")
except binascii.Error as exc:
    print("Error:", exc)
try:
    binascii.a2b_base64(b"Z")
except binascii.Error as exc:
    print("Error:", exc)
try:
    binascii.a2b_base64(b"=Zg==", strict_mode=True)
except binascii.Error as exc:
    print("Error:", exc)
try:
    binascii.a2b_base64(b"Zm$9v", strict_mode=True)
except binascii.Error as exc:
    print("Error:", exc)
try:
    binascii.a2b_base64(b"Zg==more", strict_mode=True)
except binascii.Error as exc:
    print("Error:", exc)
try:
    binascii.a2b_base64(b"Zg=Q", strict_mode=True)
except binascii.Error as exc:
    print("Error:", exc)

# --- quoted-printable ------------------------------------------------------------------
print(binascii.b2a_qp(b"hello world"))
print(binascii.b2a_qp(b"caf\xe9 = tab\t!"))
print(binascii.b2a_qp(b"tab\tspace end "))
print(binascii.b2a_qp(b"tab\tkept", quotetabs=True))
print(binascii.b2a_qp(b"line one\nline two\n"))
print(binascii.b2a_qp(b"crlf\r\nlines\r\n"))
print(binascii.b2a_qp(b"binary\r\nnewlines", istext=False))
print(binascii.b2a_qp(b"under_score head", header=True))
print(binascii.b2a_qp(b"x" * 100))
print(binascii.b2a_qp(b"trailing space \nnext"))
print(binascii.b2a_qp(b".leading dot\n.")) 
print(binascii.a2b_qp(b"caf=E9 =3D ok"))
print(binascii.a2b_qp(b"soft=\nbreak"))
print(binascii.a2b_qp(b"soft=\r\nbreak"))
print(binascii.a2b_qp(b"under_score", header=True))
print(binascii.a2b_qp(b"broken==3D"))
print(binascii.a2b_qp(b"dangling="))
print(binascii.a2b_qp(b"bad=x7"))
qp_round = binascii.a2b_qp(binascii.b2a_qp(b"caf\xe9\tmixed \nlines=\n"))
print(qp_round)

# --- uuencode ----------------------------------------------------------------------------
line = binascii.b2a_uu(b"Cat")
print(line)
print(binascii.a2b_uu(line))
print(binascii.b2a_uu(b""))
print(binascii.a2b_uu(binascii.b2a_uu(b"")) == b"")
payload = bytes(range(45))
print(binascii.a2b_uu(binascii.b2a_uu(payload)) == payload)
print(binascii.b2a_uu(b"\x00\x01\x02", backtick=True))
print(binascii.a2b_uu(binascii.b2a_uu(b"\x00\x01\x02", backtick=True)))
try:
    binascii.b2a_uu(bytes(46))
except binascii.Error as exc:
    print("Error:", exc)
try:
    binascii.a2b_uu(b"#0V%\x07\n")
except binascii.Error as exc:
    print("Error:", exc)

# --- crc32 / crc_hqx --------------------------------------------------------------------
print(binascii.crc32(b""), binascii.crc32(b"", 42))
print(binascii.crc32(b"The quick brown fox jumps over the lazy dog"))
running = binascii.crc32(b" world", binascii.crc32(b"hello"))
print(running == binascii.crc32(b"hello world"))
print(binascii.crc_hqx(b"123456789", 0))
print(binascii.crc_hqx(b"tail", binascii.crc_hqx(b"head", 0)) == binascii.crc_hqx(b"headtail", 0))

# --- argument type errors are TypeError, zero-arg calls too ------------------------------
for fn in [binascii.hexlify, binascii.a2b_base64, binascii.b2a_qp, binascii.crc32]:
    try:
        fn()
    except TypeError:
        print(fn.__name__ if hasattr(fn, "__name__") else "fn", "TypeError")
try:
    binascii.b2a_base64("not bytes")
except TypeError:
    print("b2a_base64 str TypeError")
try:
    binascii.a2b_base64("caf\xe9")
except ValueError as exc:
    print("ValueError:", exc)

# --- consumers: the email-chain modules import and run on top of this module -------------
import base64
import quopri

print(base64.b64encode(b"pon runtime"))
print(base64.b64decode(base64.b64encode(b"pon runtime")))
print(base64.b16decode(base64.b16encode(b"\xde\xad\xbe\xef")))
print(quopri.encodestring(b"caf\xe9 body"))
print(quopri.decodestring(quopri.encodestring(b"caf\xe9 body")))
