class D(dict):
    def __init__(self):
        super().__init__()
        self.x = 1

d = D()
print(d.x)
