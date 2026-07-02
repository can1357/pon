class A:
    def __init_subclass__(cls, *a, **k):
        super().__init_subclass__(*a, **k)
class B(A):
    pass
print("OK init_subclass")
