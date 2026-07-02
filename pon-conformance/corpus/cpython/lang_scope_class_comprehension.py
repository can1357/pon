# Derived from CPython v3.14.0 Lib/test/test_scope.py topics (PSF license).

def make_box():
    x = "function"

    class Box:
        x = "class"
        direct = x
        comp_values = [x for marker in [0, 1]]
        gen_values = tuple(x for marker in [0])

        def method(self):
            return x

    return Box


def comprehension_leak_probe():
    value = "outer"
    made = [value for value in [1, 2, 3]]
    return made, value


Box = make_box()
print("direct", Box.direct)
print("comp", Box.comp_values)
print("gen", Box.gen_values)
print("method", Box().method())
print("leak", comprehension_leak_probe())
print("class-x", Box.x)

