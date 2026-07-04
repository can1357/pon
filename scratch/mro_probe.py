class A: pass
class B(A): pass
print([c.__name__ for c in B.mro()])
print(type(B.mro()).__name__)
print([c.__name__ for c in type.mro(B)])
print([c.__name__ for c in int.mro()])
class C: mro = 5
print(C.mro)
