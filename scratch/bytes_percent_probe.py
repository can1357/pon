def show(name, thunk):
    try:
        result = thunk()
        print(name, "OK", type(result).__name__, repr(result))
    except BaseException as exc:
        print(name, "ERR", type(exc).__name__, str(exc))


class Bytesish:
    def __bytes__(self):
        return b"BYTESISH"


class MyBytes(bytes):
    pass


show("single_int", lambda: b"%d" % 7)
show("tuple_ints", lambda: b"%d:%i:%u" % (7, -8, 9))
show("mapping_bytes_keys", lambda: b"%(name)b:%(count)04d" % {b"name": b"pon", b"count": 5})
show("bytes_b", lambda: b"%b" % b"abc")
show("bytearray_b", lambda: b"%b" % bytearray(b"abc"))
show("memoryview_b", lambda: b"%b" % memoryview(b"abc"))
show("dunder_bytes_b", lambda: b"%b" % Bytesish())
show("bytes_s_alias", lambda: b"%s" % b"alias")
show("bytes_subclass_operand", lambda: b"%b" % MyBytes(b"sub"))
show("bytes_subclass_lhs", lambda: MyBytes(b"%b") % b"lhs")
show("bytearray_lhs", lambda: bytearray(b"%b") % b"lhs")
show("ascii_a", lambda: b"%a" % "é")
show("repr_r_oracle", lambda: b"%r" % "é")
show("oct_hex", lambda: b"%#o:%#x:%#X" % (10, 10, 10))
show("float_all", lambda: b"%.1e:%.1E:%.2f:%.2F:%.3g:%.3G" % (1.25, 1.25, 1.25, 1.25, 1234.0, 1234.0))
show("char_int", lambda: b"%c" % 65)
show("char_bytes", lambda: b"%c" % b"Z")
show("char_bytearray", lambda: b"%c" % bytearray(b"Y"))
show("literal_percent", lambda: b"%% %d" % 3)
show("width_zero_int", lambda: b"%05d" % 42)
show("precision_float", lambda: b"%.2f" % 1.234)
show("precision_bytes", lambda: b"%5.2b" % b"abcdef")
show("err_unsupported", lambda: b"%q" % 1)
show("err_b_str", lambda: b"%b" % "abc")
show("err_not_all", lambda: b"%d" % (1, 2))
show("err_not_enough", lambda: b"%d %d" % (1,))
show("err_char_str", lambda: b"%c" % "A")
show("err_char_range", lambda: b"%c" % 256)
