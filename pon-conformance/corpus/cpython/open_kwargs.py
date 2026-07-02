# open() keyword binding (the _osx_support._get_system_version shape):
# the encoding keyword with positional/keyword mode combos, write/read
# round-trips, newline='' pass-through, binary-mode combo ValueErrors, and
# the TypeError leg for a bad keyword name.  io.open shares the native
# entry, so the same binder row serves both spellings.
#
# The scratch file is workspace-relative (the runner's CWD has target/, the
# file_io.py convention) so pon and the host python3.14 see identical paths.
# Bad-keyword suggestions ("Did you mean 'encoding'?") only fire for
# near-miss names, so the TypeError leg uses a name with no close match.
# newline='' line SPLITTING of lone-\r endings is not implemented (pon's
# pass-through mode splits only on \n); content here sticks to \n and \r\n,
# where CPython's splitter agrees.

path = "target/pon_open_kwargs_corpus.txt"

# mode positional + encoding keyword: the _get_system_version write twin.
f = open(path, "w", encoding="utf-8")
print(f.write("alpha\nbeta\n"))
f.close()

# file positional + encoding keyword only (mode defaults to 'r').
with open(path, encoding="utf-8") as f:
    print(f.read() == "alpha\nbeta\n")

# every-argument-by-keyword spelling.
with open(file=path, mode="r", encoding="utf-8") as f:
    print(f.readline())

# mode= and encoding= both as keywords, append leg.
f = open(path, mode="a", encoding="utf-8")
print(f.write("gamma\n"))
f.close()
with open(path, mode="r", encoding="utf-8") as f:
    print(f.read() == "alpha\nbeta\ngamma\n")

# Default-valued keyword slots bind explicitly: buffering/closefd/opener.
with open(path, "r", buffering=-1, encoding="utf-8", closefd=True, opener=None) as f:
    print(f.readline() == "alpha\n")

# newline='' write pass-through: \r\n survives verbatim (checked over the
# binary spelling); reading with newline='' keeps it untranslated while the
# newline=None default translates.
with open(path, "w", encoding="utf-8", newline="") as f:
    print(f.write("one\r\ntwo\n"))
with open(path, "rb") as f:
    print(f.read() == b"one\r\ntwo\n")
with open(path, "r", encoding="utf-8", newline="") as f:
    print(f.readline() == "one\r\n")
    print(f.read() == "two\n")
with open(path, encoding="utf-8") as f:
    print(f.read() == "one\ntwo\n")

# errors='strict' is the (default) supported handler in text mode.
with open(path, "r", encoding="utf-8", errors="strict") as f:
    print(len(f.read()))

# TypeError leg: bad keyword name (no near-miss, so CPython's suggestion
# machinery stays quiet and the message is differential-stable).
try:
    open(path, bogus=True)
except TypeError as exc:
    print("TypeError:", exc)

# A keyword duplicating a positional is also a TypeError; CPython's
# C-function wording differs from the binder's, so only the type is pinned.
try:
    open(path, "r", mode="w")
except TypeError as exc:
    print("dup", type(exc).__name__)

# Binary-mode keyword combos are ValueErrors with CPython's wording.
try:
    open(path, "rb", encoding="utf-8")
except ValueError as exc:
    print("ValueError:", exc)
try:
    open(path, "rb", errors="strict")
except ValueError as exc:
    print("ValueError:", exc)
try:
    open(path, "rb", newline="\n")
except ValueError as exc:
    print("ValueError:", exc)

# io.open shares the native entry: the keyword row serves both spellings.
import io

with io.open(path, encoding="utf-8") as f:
    print("io", f.read() == "one\ntwo\n")
