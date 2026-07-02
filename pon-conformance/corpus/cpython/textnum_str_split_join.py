# Derived from CPython v3.14.0 Lib/test/test_str.py topics (PSF license).

def show_words(label, text):
    pieces = text.split()
    print(label, len(pieces))
    print(pieces)


def show_sep(label, text, sep):
    pieces = text.split(sep)
    print(label, len(pieces))
    print(pieces[0])
    print(pieces[-1])
    print(pieces)


def show_join(label, sep, pieces):
    joined = sep.join(pieces)
    print(label, joined)
    print(len(joined))


show_words("plain", "  alpha beta  gamma ")
show_words("mixed", "\talpha\n\nbeta\r\ngamma  ")
show_words("single", " solitary ")
show_words("empty", "   ")

show_sep("comma", "red,,green,blue,", ",")
show_sep("double", "left--middle--right", "--")
show_sep("unicode", "αβγβδ", "β")
show_sep("missing", "no delimiters here", "|")

show_join("dash-list", "-", ["pon", "py", "314"])
show_join("empty-tuple", "", ("a", "b", "c"))
show_join("wide", "::", ["left", "", "right"])
show_join("slash", "/", ["left", "middle", "right"])
