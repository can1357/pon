# unicodedata surface: normalize (all four UAX #15 forms over mixed scripts,
# combining-mark reordering, and algorithmic Hangul), the category matrix,
# combining classes, east_asian_width, and the decimal/digit/numeric triple
# including their error legs.  Everything prints as ASCII codepoint spellings
# or exact exception text, so the output is differential-stable byte-for-byte
# against the host oracle.
import unicodedata

print(unicodedata.unidata_version)


def cps(text):
    return " ".join("U+%04X" % ord(ch) for ch in text) or "-"


FORMS = ("NFC", "NFD", "NFKC", "NFKD")

BATTERY = (
    "abc",                            # ASCII fixed point
    "-\u00e0\u00f2\u0258\u0141\u011f",  # os_helper:31 TESTFN_UNICODE tail
    "\u00c5\u212b\u2126\u03a9",       # singletons: Angstrom/Ohm vs letters
    "a\u0300",                        # combine to a-grave
    "s\u0323\u0307",                  # multi-mark composition (s-dot-below+above)
    "s\u0307\u0323",                  # same marks, swapped input order
    "q\u0307\u0323",                  # no composite exists; ordering only
    "\u1e69",                         # decomposes two levels down
    "\u0344",                         # composition exclusion (non-starter decomp)
    "\u1ebf\u01b0\u1edd",             # Vietnamese stacked diacritics
    "\u0958\u095b",                   # Devanagari composition exclusions
    "\u0397\u0301\u03ae",             # Greek eta with tonos, both spellings
    "\u0419\u0439",                   # Cyrillic short I
    "\ufb01\ufb03",                   # Latin ligatures (compat only)
    "\u2460\u2075\u00bd",             # circled/superscript/vulgar fraction
    "\uff21\uff4d\uff01",             # full-width forms
    "\u2162\u216c",                   # Roman numerals
    "\ufdfa",                         # widest compat expansion (18 cps)
    "\u3384\u33a0",                   # squared unit symbols
    "\uac00\uac01\ud7a3",             # Hangul LV / LVT / last syllable
    "\ud55c\uae00",                   # "Hangul" as syllables
    "\u1112\u1161\u11ab",             # Jamo sequence composing to a syllable
    "\u1100\u1161",                   # Jamo LV pair
    "\uac00\u11a8",                   # LV syllable + trailing T jamo
    "\u3131\u314f",                   # compat Jamo (NFKD to real Jamo)
    "e\u0301\u0327",                  # blocked vs unblocked mark orders
    "e\u0327\u0301",
    "\u212b\u0300",                   # singleton then mark
    "\u0061\u05b0\u0591",             # Hebrew points reorder by ccc
)

for text in BATTERY:
    print(cps(text), "->", " | ".join(form + " " + cps(unicodedata.normalize(form, text)) for form in FORMS))

# Idempotence: normalizing a normalized string is a fixed point.
print(all(
    unicodedata.normalize(form, unicodedata.normalize(form, text)) == unicodedata.normalize(form, text)
    for form in FORMS
    for text in BATTERY
))

# Category matrix across the major classes (letters, marks, numbers,
# punctuation, symbols, separators, controls, format, surrogates-adjacent,
# private use, unassigned).
for ch in (
    "A", "a", "\u01c5", "\u02b0", "\u4e00",
    "\u0301", "\u0903", "\u20dd",
    "7", "\u2162", "\u00bd",
    "_", "-", "(", ")", "\u00ab", "\u00bb", "!",
    "+", "$", "^", "\u00a9",
    " ", "\u2028", "\u2029",
    "\x00", "\xad", "\u200d",
    "\ue000", "\u0378", "\U0010fffd", "\U0010ffff",
):
    print("U+%04X" % ord(ch), unicodedata.category(ch))

# Combining classes: 0 default, common above/below marks, Hebrew/Arabic
# fixed-position classes, Hangul jamo (0), kana voicing.
for ch in ("a", "\u0300", "\u0301", "\u0323", "\u0334", "\u05b0", "\u0591", "\u0651", "\u3099", "\u1100", "\u20dd"):
    print("U+%04X" % ord(ch), unicodedata.combining(ch))

# East-Asian width: one of each class.
for ch in ("a", "\u00a1", "\u1100", "\u4e00", "\uff01", "\uff61", "\u3000", "\x00", "\u2028", "\U0001f600", "\U000e0001"):
    print("U+%04X" % ord(ch), unicodedata.east_asian_width(ch))

# decimal / digit / numeric values and their disjointness tiers:
# '7' has all three, '①' starts at digit, '½' only numeric.
for ch in ("7", "\u0665", "\u0966", "\uff17", "\u2460", "\u00b2", "\u00bd", "\u5341", "\u216c", "\u3007"):
    row = ["U+%04X" % ord(ch)]
    for probe in (unicodedata.decimal, unicodedata.digit, unicodedata.numeric):
        try:
            row.append(repr(probe(ch)))
        except ValueError as exc:
            row.append("ValueError(%s)" % exc)
    print(" ".join(row))

# Defaults suppress the ValueError legs.
print(unicodedata.decimal("a", None), unicodedata.digit("a", -1), unicodedata.numeric("a", "none"))

# Exact error text parity: wrong-type / wrong-length converter errors, the
# METH_O and clinic arity shapes, and the normalize form/type errors.
for thunk in (
    lambda: unicodedata.category(7),
    lambda: unicodedata.category("ab"),
    lambda: unicodedata.category(""),
    lambda: unicodedata.category(),
    lambda: unicodedata.combining("xy"),
    lambda: unicodedata.east_asian_width(None),
    lambda: unicodedata.decimal(7),
    lambda: unicodedata.decimal("ab"),
    lambda: unicodedata.decimal(),
    lambda: unicodedata.decimal("a", 1, 2),
    lambda: unicodedata.numeric("\u4e00"),
    lambda: unicodedata.digit("\u00bd"),
    lambda: unicodedata.normalize("NFX", "a"),
    lambda: unicodedata.normalize("nfc", "a"),
    lambda: unicodedata.normalize(7, "a"),
    lambda: unicodedata.normalize("NFC", 7),
    lambda: unicodedata.normalize("NFC"),
):
    try:
        thunk()
        print("no error")
    except (TypeError, ValueError) as exc:
        print(type(exc).__name__, str(exc))
