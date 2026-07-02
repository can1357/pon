# Complex constructor, arithmetic, attributes, abs, and repr canaries.

def show_construct(text):
    try:
        value = complex(text)
        print("construct", repr(text), repr(value), value.real, value.imag)
    except ValueError as exc:
        print("construct-error", repr(text), type(exc).__name__, str(exc))


show_construct("3+4j")
show_construct("(3+4j)")
show_construct("  -2.5j  ")
show_construct("nan+infj")
show_construct("-inf-infj")
show_construct("1 + 2j")

z = complex(3, 4)
w = complex(-2, 0.5)
print("parts", z.real, z.imag, w.real, w.imag)
print("arith", z + w, z - w, z * w, z / w)
print("unary", -z, +z, z.conjugate())
print("abs", abs(z), abs(complex(-5, 12)))
print("repr", repr(complex(0, 2)), repr(complex(1, 2)), repr(complex(-0.0, -0.0)))
print("paren", str([complex(1, 2), complex(0, 2)]))
print("compare", complex(1, 2) == complex(1.0, 2.0), complex(1, 2) != complex(2, 1))
