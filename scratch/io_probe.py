import io
import os

TEXT = "/tmp/pon_io_probe_text.txt"
BIN = "/tmp/pon_io_probe_bin.dat"
APP = "/tmp/pon_io_probe_append.txt"
NL = "/tmp/pon_io_probe_newline.txt"


def exc(label, func):
    try:
        value = func()
        print(label, "OK", value)
    except Exception as e:
        print(label, type(e).__name__, isinstance(e, io.UnsupportedOperation), str(e))


# text write, writelines, flush, context manager, mode/name/encoding flags
with open(TEXT, "w", encoding="utf-8") as f:
    print("text_meta", f.name == TEXT, f.mode, f.encoding.lower(), f.errors, f.readable(), f.writable(), f.seekable(), f.isatty(), f.closed, f.newlines)
    print("text_write", f.write("alpha\n"))
    print("text_writelines", f.writelines(["beta\r\n", "gamma\r", "delta"]))
    print("text_tell_after_write", f.tell() > 0)
    print("text_flush", f.flush())
print("text_closed_after_with", f.closed)

# text read, read(n), readline(n), readline(), readlines(), iteration, next, tell/seek
with open(TEXT, "r", encoding="utf-8", newline=None) as f:
    print("text_read_flags", f.readable(), f.writable(), f.seekable(), f.isatty(), f.closed, f.newlines)
    print("text_read_chars", f.read(5), f.tell())
    print("text_seek_start", f.seek(0), f.tell())
    print("text_readline_limit", repr(f.readline(3)), f.tell())
    print("text_readline_rest", repr(f.readline()))
    print("text_read_rest", repr(f.read()))
with open(TEXT, "r", encoding="utf-8") as f:
    print("text_readlines", f.readlines())
with open(TEXT, "r", encoding="utf-8") as f:
    print("text_readlines_hint", f.readlines(8))
with open(TEXT, "r", encoding="utf-8") as f:
    print("text_iter", [line for line in f])
with open(TEXT, "r", encoding="utf-8") as f:
    print("text_next", next(f), f.__next__())
    print("text_seek_end", f.seek(0, 2) >= 0, f.tell() >= 0)

# newline modes: universal translate, universal preserve, explicit write translation
open(NL, "wb").write(b"a\r\nb\rc\n")
with open(NL, "r", encoding="utf-8", newline=None) as f:
    print("nl_none", repr(f.read()), f.newlines)
with open(NL, "r", encoding="utf-8", newline="") as f:
    print("nl_empty", repr(f.read()), f.newlines)
with open(NL, "r", encoding="utf-8", newline="\r") as f:
    print("nl_cr_lines", repr(f.readline()), repr(f.readline()))
with open(NL, "w", encoding="utf-8", newline="\r\n") as f:
    print("nl_write_count", f.write("x\ny\n"))
with open(NL, "rb") as f:
    print("nl_write_bytes", f.read())

# append mode
open(APP, "w").write("base")
with open(APP, "a", encoding="utf-8") as f:
    print("append_write", f.write("+tail"), f.tell() >= 0)
with open(APP, "r", encoding="utf-8") as f:
    print("append_result", f.read())

# binary write/read/read1/readinto/readlines/iteration/tell/seek/mode attrs
with open(BIN, "wb") as f:
    print("bin_meta", f.name == BIN, f.mode, hasattr(f, "encoding"), f.readable(), f.writable(), f.seekable(), f.isatty(), f.closed)
    print("bin_write", f.write(b"one\ntwo\nthree"))
    print("bin_writelines", f.writelines([bytearray(b"\nfour"), b"\nfive"]))
    print("bin_flush", f.flush())
with open(BIN, "rb") as f:
    print("bin_read", f.read(3), f.tell())
    print("bin_seek", f.seek(0), f.tell())
    print("bin_readline", f.readline(), f.readline(2), f.readline())
    print("bin_read1", f.read1(4), f.read1())
with open(BIN, "rb") as f:
    buf = bytearray(5)
    print("bin_readinto", f.readinto(buf), bytes(buf), f.tell())
    buf2 = bytearray(100)
    print("bin_readinto_eof", f.readinto(buf2) > 0, bytes(buf2[:3]))
with open(BIN, "rb") as f:
    print("bin_readlines", f.readlines())
with open(BIN, "rb") as f:
    print("bin_iter", [line for line in f])

# wrong-direction and wrong-type failures
fw = open(TEXT, "w", encoding="utf-8")
exc("err_read_writeonly", lambda: fw.read())
fw.close()
fr = open(TEXT, "r", encoding="utf-8")
exc("err_write_readonly", lambda: fr.write("x"))
fr.close()
ft = open(TEXT, "w", encoding="utf-8")
exc("err_bytes_to_text", lambda: ft.write(b"x"))
ft.close()
fb = open(BIN, "wb")
exc("err_str_to_binary", lambda: fb.write("x"))
fb.close()
with open(TEXT, "r", encoding="utf-8") as f:
    print("text_has_binary_methods", hasattr(f, "readinto"), hasattr(f, "read1"))

# closed file behavior and double close
fc = open(TEXT, "r", encoding="utf-8")
fc.close()
fc.close()
print("closed_double", fc.closed)
exc("closed_read", lambda: fc.read())
exc("closed_write", lambda: fc.write("x"))
exc("closed_seek", lambda: fc.seek(0))
exc("closed_tell", lambda: fc.tell())
exc("closed_flush", lambda: fc.flush())
exc("closed_readable", lambda: fc.readable())
exc("closed_writable", lambda: fc.writable())
exc("closed_seekable", lambda: fc.seekable())
exc("closed_fileno", lambda: fc.fileno())
exc("closed_isatty", lambda: fc.isatty())
exc("closed_next", lambda: next(fc))

# fd path over pipes: binary and text wrappers own/close the fds
r, w = os.pipe()
wf = open(w, "wb")
print("pipe_write_flags", wf.writable(), wf.readable(), wf.seekable())
print("pipe_write", wf.write(b"pipe-bytes\n"))
wf.close()
rf = open(r, "rb")
print("pipe_read_flags", rf.readable(), rf.writable(), rf.seekable())
print("pipe_read", rf.read())
rf.close()

r, w = os.pipe()
wf = open(w, "wb")
wf.write(b"line1\nline2\n")
wf.close()
tf = open(r, "r", encoding="utf-8")
print("pipe_text", tf.readline(), tf.read())
tf.close()
