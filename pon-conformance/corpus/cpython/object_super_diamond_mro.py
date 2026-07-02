# Derived from CPython v3.14.0 Lib/test/test_super.py topics (PSF license).

class A:
    def f(self):
        return "A"


class B(A):
    def f(self):
        return super().f() + "B"


class C(A):
    def f(self):
        return super().f() + "C"


class D(C, B):
    def f(self):
        return super().f() + "D"


class E(D):
    pass


d = D()
e = E()
print(d.f())
print(D.f(d))
print(e.f())
print(d.f() == e.f())
print(isinstance(e, D))
print(isinstance(e, A))
