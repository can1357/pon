class S:
    def __getitem__(self, i):
        if i > 2:
            raise IndexError
        return i * 10


for x in S():
    print(x)
