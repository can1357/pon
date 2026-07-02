import helper_doc
print(repr(getattr(helper_doc, "__doc__", "MISSING")))
print(repr(getattr(helper_doc, "__file__", "MISSING")))
print(type(dir(5)) is list, len(dir(5)) >= 0)
print(type(dir("x")) is list)
class C:
    x = 1
print("x" in dir(C))
c = C()
print("x" in dir(c))
