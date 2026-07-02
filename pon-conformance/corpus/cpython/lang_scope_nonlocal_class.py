# Derived from CPython v3.14.0 Lib/test/test_scope.py topics (PSF license).

def make_counter(start):
    class Counter:
        nonlocal start
        start += 1
        label = "counter"

        def read(self):
            return start

        def bump(self):
            nonlocal start
            start += 10
            return start

    return Counter()


first = make_counter(5)
print("first initial", first.read(), first.__class__.label, hasattr(first.__class__, "start"))
print("first bump", first.bump(), first.read())
print("first bump again", first.bump(), first.read())
second = make_counter(-1)
print("second initial", second.read(), second.__class__.label, hasattr(second.__class__, "start"))
print("second bump", second.bump(), second.read())
third = make_counter(0)
print("third initial", third.read(), third.__class__.label, hasattr(third.__class__, "start"))
print("third bump", third.bump(), third.read())

