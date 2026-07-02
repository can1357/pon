# Derived from CPython v3.14.0 Lib/test/test_format.py topics (PSF license).

def show(label, value, spec):
    print(label, format(value, spec))


show("int-zero", 42, "04d")
show("int-wide", 42, ">6d")
show("int-left", 42, "<6d")
show("int-center", 42, "*^6d")
show("int-neg-zero", -42, "05d")
show("bool-true", True, "d")
show("bool-false", False, "04d")

show("str-left", "abc", "<6s")
show("str-right", "abc", ">6s")
show("str-center", "abc", "*^7s")
show("str-prec", "abcdef", ".3s")
show("str-wide-prec", "abcdef", ">6.3s")
show("str-untyped", "abcdef", ">8.4")

show("float-default", 1.25, "f")
show("float-prec", 3.14159, ".2f")
show("float-wide", 3.5, "08.2f")
show("float-neg", -3.5, "08.2f")
show("float-text", -0.0, ".1f")

print(format("pon", "*^7s") + ":" + format(7, "03d"))
print(format(1.0 / 8.0, ".3f"))
print(format("xy", ">4s") + ":" + format(2.5, ".1f"))
