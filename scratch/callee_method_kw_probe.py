class C:
    def f(self, *, reconfigure=False):
        print('f called', reconfigure)

c = C()
m = c.f
print(type(m), repr(m))
m(reconfigure=False)
print('ok')
