class Meta(type):
    pass

class A(metaclass=Meta):
    def __ne__(self, other):
        return "ne"
    def own(self):
        return "own"

class B(A, metaclass=Meta):
    pass

class C:
    def __ne__(self, other):
        return "ne2"

class D(C):
    pass

print(A.own, A.__ne__)
print(B.own)
print(B.__ne__)
print(D.__ne__)
