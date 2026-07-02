# Derived from CPython v3.14.0 Lib/test/test_str.py topics (PSF license).

def show_strip(label, text):
    stripped = text.strip()
    print(label, stripped)
    print(len(stripped))


def show_case(label, text):
    print(label)
    print(text.lower())
    print(text.upper())
    print(text.title())


show_strip("spaces", "   padded   ")
show_strip("tabs", "\t\n padded\r\n")
show_strip("none", "already clean")
show_strip("empty", " \t\n ")

show_case("ascii", "miXeD words")
show_case("apostrophe", "they're bill's")
show_case("hyphen", "alpha-beta gamma")
show_case("unicode", "mañana café")

combo = "  hello WORLD  "
print(combo.strip().lower())
print(combo.strip().upper())
print(combo.strip().title())

name = "pon conformance"
print(name.title().startswith("Pon"))
print(name.upper().endswith("ANCE"))
