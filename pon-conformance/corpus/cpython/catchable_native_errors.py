# Typed, catchable exceptions from native str/bytes/seq/dict error paths:
# `except <Type>:` must fire wherever CPython raises <Type> (message-only
# diagnostics are uncatchable).  Messages print only where pon's text matches
# CPython byte-for-byte; other cases assert the exception type alone.


def t(label, fn, exc):
    try:
        fn()
        print(label, "no-error")
    except exc as e:
        print(label, "caught", type(e).__name__)


def tm(label, fn, exc):
    try:
        fn()
        print(label, "no-error")
    except exc as e:
        print(label, "caught", type(e).__name__, str(e))


print("== str ==")
tm("str-index-oob", lambda: "abc"[5], IndexError)
tm("str-index-missing", lambda: "ab".index("z"), ValueError)
tm("str-split-empty", lambda: "a b".split(""), ValueError)
tm("str-partition-empty", lambda: "ab".partition(""), ValueError)
tm("str-slice-step0", lambda: "abc"[::0], ValueError)
tm("str-translate-range", lambda: "a".translate({97: 0x110000}), ValueError)
tm("str-maketrans-len", lambda: "".maketrans("ab", "c"), ValueError)
t("str-zfill-arity", lambda: "a".zfill(), TypeError)
t("str-strip-int", lambda: "a".strip(1), TypeError)
t("str-join-nonstr", lambda: ",".join([1]), TypeError)
t("str-center-fill", lambda: "a".center(5, "xy"), TypeError)
_s = "ab"
t("str-subscript-str", lambda: _s["x"], TypeError)
t("str-attr-miss", lambda: "a".nope, AttributeError)
print("hasattr-str", hasattr("a", "nope"))
print("getattr-default", getattr("a", "nope", "fallback"))

print("== bytes ==")
tm("bytes-index-oob", lambda: b"ab"[5], IndexError)
tm("bytes-index-missing", lambda: b"ab".index(b"z"), ValueError)
tm("bytes-count-256", lambda: b"ab".count(256), ValueError)
tm("bytes-fromhex-odd", lambda: b"".fromhex("z"), ValueError)
tm("bytearray-pop-empty", lambda: bytearray().pop(), IndexError)
t("bytearray-pop-oob", lambda: bytearray(b"a").pop(5), IndexError)
tm("bytearray-remove-missing", lambda: bytearray(b"a").remove(99), ValueError)
tm("bytearray-append-256", lambda: bytearray(b"a").append(256), ValueError)
t("bytearray-append-str", lambda: bytearray(b"a").append("x"), TypeError)
tm("bytes-ctor-negative", lambda: bytes(-1), ValueError)


def _ba_store():
    b = bytearray(b"a")
    b[0] = 256


tm("bytearray-store-256", _ba_store, ValueError)


def _ba_extslice():
    b = bytearray(b"abcd")
    b[0:4:2] = b"xyz"


t("bytearray-extslice", _ba_extslice, ValueError)

print("== seq ==")
t("list-index-oob", lambda: [1][5], IndexError)
t("list-pop-empty", lambda: [].pop(), IndexError)
t("list-index-missing", lambda: [].index(1), ValueError)
t("list-remove-missing", lambda: [1].remove(2), ValueError)
_xs = [1]

def _call_int():
    n = 0
    return n()


t("call-int", _call_int, TypeError)
t("list-subscript-str", lambda: _xs["x"], TypeError)
tm("list-slice-step0", lambda: [1, 2][::0], ValueError)
t("tuple-concat-list", lambda: (1,) + [2], TypeError)
t("list-mul-str", lambda: [1] * "x", TypeError)
tm("range-str", lambda: range("a"), TypeError)
t("sort-mixed", lambda: [1, "a"].sort(), TypeError)
t("list-clear-arity", lambda: [1].clear(1), TypeError)
t("list-attr-miss", lambda: [].nope, AttributeError)

print("== dict/set ==")
tm("dict-missing-key", lambda: {}["k"], KeyError)
t("dict-get-arity", lambda: {}.get(), TypeError)
t("dict-unhashable-key", lambda: {}.__setitem__([1], 2), TypeError)
tm("set-remove-missing", lambda: set().remove(1), KeyError)
tm("set-pop-empty", lambda: set().pop(), KeyError)
t("set-add-arity", lambda: set().add(), TypeError)
t("dict-attr-miss", lambda: {}.nope, AttributeError)

print("== base classes ==")
try:
    "a".zfill()
except Exception as e:
    print("except-Exception caught", type(e).__name__)
try:
    "abc"[9]
except LookupError as e:
    print("except-LookupError caught", type(e).__name__)
print("done")
