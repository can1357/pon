import typing as T
SubProject = T.NewType('SubProject', str)
print('newtype call:', repr(SubProject('')))
class F:
    def __call__(self, x): return ('called', x)
f = F()
print('instance call:', f(5))
def g(a, b=SubProject('x')): return b
print('default arg:', repr(g(1)))
class C:
    @staticmethod
    def h(s=SubProject('y')): return s
print('class default:', repr(C.h()))
