class F:
    def __call__(self, x): return ('called', x)
print(F()(5))
