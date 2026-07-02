class D(dict):
    def __setitem__(self, k, v):
        super().__setitem__(k, v.upper() if isinstance(v, str) else v)

d = D()
d["a"] = "hi"
d["b"] = 3
print(d["a"], d["b"])
print(sorted(d.keys()))
