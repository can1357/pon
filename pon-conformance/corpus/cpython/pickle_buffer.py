import pickle
from pickle import PickleBuffer

# --- construction + bytes round-trip ----------------------------------------------
pb = PickleBuffer(b"pon payload")
print(type(pb).__name__, type(pb).__module__)
print(PickleBuffer, PickleBuffer.__name__, PickleBuffer.__module__)
print(pickle.PickleBuffer is PickleBuffer)
print(bytes(pb))
mv = memoryview(pb)
print(mv.nbytes, mv.format, mv.itemsize, mv.ndim, mv.readonly, mv.contiguous)
print(mv.tobytes())
print(bytes(mv), mv[0], mv[-1], bytes(mv[4:7]))
mv.release()

# --- raw() ---------------------------------------------------------------------------
pb = PickleBuffer(b"\x00\x01\x02rawtail")
raw = pb.raw()
print(raw.nbytes, raw.format, raw.tobytes())
raw.release()

# --- bytearray source stays writable through the exported view ------------------------
buf = bytearray(b"mutable!")
view = memoryview(PickleBuffer(buf))
print(view.readonly, view.tobytes())
view[0] = ord("M")
print(buf)
view.release()

# --- release semantics ------------------------------------------------------------------
pb = PickleBuffer(b"gone")
pb.release()
try:
    memoryview(pb)
except ValueError as exc:
    print("ValueError:", exc)
try:
    pb.raw()
except ValueError as exc:
    print("ValueError:", exc)
pb.release()
print("release-idempotent-ok")

# --- error legs ---------------------------------------------------------------------------
try:
    PickleBuffer(1)
except TypeError as exc:
    print("TypeError:", exc)

# --- with-statement over the exported view --------------------------------------------------
with memoryview(PickleBuffer(b"ctx")) as ctx_view:
    print(ctx_view.tobytes())

# Protocol-5 dumps/loads round-trips (buffer_callback / buffers=) are the
# next frontier: the pure-Python pickler needs a writable io.BytesIO, and
# pon's native `_io` write path is not implemented yet.
