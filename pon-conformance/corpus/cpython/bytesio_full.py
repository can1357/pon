# Real io.BytesIO: construction, write/read/seek/tell round-trips, getbuffer
# aliasing with CPython's export pinning (BufferError), truncate semantics,
# closed-file errors, iteration, readinto, and pickle protocol-5 end-to-end
# (framed dumps/loads plus PEP 574 out-of-band buffer_callback/buffers).
import io
import pickle

# --- construction ----------------------------------------------------------
b = io.BytesIO()
print(b.getvalue(), b.tell())
b = io.BytesIO(b"abc")
print(b.getvalue(), b.tell(), len(b.getvalue()))
print(io.BytesIO(initial_bytes=b"kw").getvalue())
print(io.BytesIO(bytearray(b"ba")).getvalue())
print(io.BytesIO(memoryview(b"mv")).getvalue())
print(io.BytesIO(None).getvalue())
print(type(io.BytesIO).__name__, io.BytesIO.__name__, io.BytesIO.__module__)
print(isinstance(b, io.BytesIO), issubclass(io.BytesIO, io.BufferedIOBase))
try:
    io.BytesIO("s")
except TypeError as e:
    print("TypeError:", e)

# --- write/read/seek/tell round-trips --------------------------------------
b = io.BytesIO()
print(b.write(b"hello"), b.tell())
print(b.write(b""), b.write(bytearray(b" world")), b.tell())
print(b.write(memoryview(b"!")))
print(b.getvalue())
print(b.seek(0), b.read(5), b.tell())
print(b.read(0), b.read(), b.read(), b.read(3))
print(b.seek(2, 0), b.seek(2, 1), b.seek(-3, 2), b.tell())
print(b.read(100))
try:
    b.seek(-1)
except ValueError as e:
    print("ValueError:", e)
print(b.seek(-100, 1), b.seek(-100, 2))
try:
    b.seek(0, 3)
except ValueError as e:
    print("ValueError:", e)
try:
    b.seek(1.0)
except TypeError as e:
    print("TypeError:", e)
try:
    b.write("text")
except TypeError as e:
    print("TypeError:", e)

# seek past EOF: reads see empty, writes zero-fill the gap
b = io.BytesIO(b"ab")
print(b.seek(5), b.tell(), b.read(), b.getvalue())
print(b.write(b"z"), b.getvalue())
b = io.BytesIO(b"abcdef")
b.seek(2)
print(b.write(b"XY"), b.getvalue(), b.tell())

# --- readline / readlines / iteration / read1 ------------------------------
b = io.BytesIO(b"one\ntwo\nthree")
print(b.readline(), b.readline(2), b.readline(), b.readline(), b.readline())
b.seek(0)
print(b.readlines())
b.seek(0)
print(b.readlines(4))
b.seek(0)
print(list(b))
b.seek(0)
print(b.read1(4), b.read1(), b.read1(2))

# --- readinto ---------------------------------------------------------------
b = io.BytesIO(b"payload")
buf = bytearray(4)
print(b.readinto(buf), buf)
buf2 = bytearray(10)
print(b.readinto(buf2), buf2)
print(b.readinto(bytearray()))
b.seek(0)
mv = memoryview(bytearray(3))
print(b.readinto(mv), bytes(mv))

# --- truncate ----------------------------------------------------------------
b = io.BytesIO(b"abcdef")
print(b.seek(2), b.truncate(), b.getvalue(), b.tell())
print(b.truncate(1), b.getvalue(), b.tell())
b = io.BytesIO(b"abc")
print(b.truncate(100), b.getvalue(), b.tell())
try:
    b.truncate(-1)
except ValueError as e:
    print("ValueError:", e)

# --- stream flags -------------------------------------------------------------
b = io.BytesIO()
print(b.readable(), b.writable(), b.seekable(), b.isatty(), b.closed)

# --- getbuffer aliasing + export pinning + release -----------------------------
b = io.BytesIO(b"abcdef")
v = b.getbuffer()
print(type(v).__name__, len(v), v.readonly, v.nbytes, bytes(v))
v[0] = ord("X")
print(b.getvalue())
v[1:3] = b"YZ"
print(b.getvalue())
print(b.read(2))
try:
    b.write(b"q")
except BufferError as e:
    print("BufferError:", e)
try:
    b.truncate(2)
except BufferError as e:
    print("BufferError:", e)
try:
    b.close()
except BufferError as e:
    print("BufferError:", e)
print(b.tell(), b.seek(0), b.getvalue())
v.release()
print(b.write(b"ok"), b.getvalue())
v2 = b.getbuffer()
v3 = b.getbuffer()
v2.release()
try:
    b.truncate(1)
except BufferError as e:
    print("BufferError:", e)
v3.release()
v3.release()
print(b.truncate(1), b.getvalue())

# --- close semantics -----------------------------------------------------------
b = io.BytesIO(b"bye")
b.close()
b.close()
print(b.closed)
for op in ("getvalue", "read", "write", "seek", "tell", "truncate", "readline",
           "readlines", "getbuffer", "read1", "flush", "readable", "writable",
           "seekable", "isatty"):
    try:
        m = getattr(b, op)
        if op == "write":
            m(b"x")
        elif op == "seek":
            m(0)
        else:
            m()
    except ValueError as e:
        print(op, "ValueError:", e)

with io.BytesIO(b"ctx") as w:
    print(w.read())
print(w.closed)
b = io.BytesIO()
print(b.flush())

# --- pickle end-to-end -----------------------------------------------------------
# Protocol 0 is omitted (its str opcode needs the raw-unicode-escape codec,
# which pon's native _codecs does not provide yet); bytes payloads join at
# protocol 3+ (below that pickle encodes bytes via a `codecs.encode` GLOBAL
# reference, and pickling native functions by reference is a separate
# frontier).  Framing (protocol 4+) drives commit_frame's getbuffer() ->
# write(memoryview) handoff through BytesIO.
for proto in (1, 2, 3, 4, 5):
    data = {"k": [1, 2, 3], "v": ("nested", 3.5), "flag": True}
    if proto >= 3:
        data["raw"] = b"bytes"
    blob = pickle.dumps(data, protocol=proto)
    print(proto, type(blob).__name__, pickle.loads(blob) == data)

# larger-than-frame payload exercises commit_frame + getbuffer handoff
big = {"payload": bytes(range(256)) * 300, "tail": "end"}
blob = pickle.dumps(big, protocol=5)
print(len(blob) > 60000, pickle.loads(blob) == big)

# file-object dump/load through an explicit BytesIO
f = io.BytesIO()
pickle.dump([1, "two", b"three"], f, protocol=5)
f.seek(0)
print(pickle.load(f))

# out-of-band buffers (PEP 574): buffer_callback + buffers=
buffers = []
payload = {"blob": pickle.PickleBuffer(bytearray(b"outofband-payload")), "n": 7}
blob = pickle.dumps(payload, protocol=5, buffer_callback=buffers.append)
print(len(buffers), all(type(pb).__name__ == "PickleBuffer" for pb in buffers))
restored = pickle.loads(blob, buffers=[pb.raw() for pb in buffers])
print(type(restored["blob"]).__name__, bytes(restored["blob"]), restored["n"])

# readonly (bytes-backed) PickleBuffer leg -> READONLY_BUFFER opcode
buffers2 = []
blob2 = pickle.dumps(pickle.PickleBuffer(b"readonly-payload"), protocol=5,
                     buffer_callback=buffers2.append)
restored2 = pickle.loads(blob2, buffers=buffers2)
print(type(restored2).__name__, bytes(restored2))
