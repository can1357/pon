# tb_lineno is real at statement granularity: the raise-site entry carries the
# raise statement's line, and every outer frame reports the call statement it
# was executing.  All catches sit at module level so the CPython chain and
# pon's raise-time snapshot cover the same frames.


def inner():
    raise ValueError("boom")


def outer():
    inner()


def walk(exc):
    lines = []
    tb = exc.__traceback__
    while tb is not None:
        lines.append(tb.tb_lineno)
        tb = tb.tb_next
    return lines


# Raise through two compiled frames: module entry at the `outer()` call line,
# outer at the `inner()` call line, raise site last.
try:
    outer()
except ValueError as caught:
    print("two frames", walk(caught))

# Re-raising the same instance at a new line prepends a fresh module-level
# entry; the original raise line survives as the deeper suffix.
try:
    try:
        raise KeyError("k")
    except KeyError as second:
        raise second
except KeyError as reraised:
    print("re-raise", walk(reraised))

# A raise inside an except handler records the handler-body raise line.
try:
    try:
        raise IndexError("first")
    except IndexError:
        raise RuntimeError("second")
except RuntimeError as handler_exc:
    print("handler raise", walk(handler_exc))

# Implicit raises (failed name lookup) attribute to their statement's line.
try:
    unbound_name
except NameError as name_exc:
    print("implicit", walk(name_exc))
