# Derived from CPython v3.14.0 Lib/test/test_descr.py topics (PSF license).

class SequenceLike:
    def __init__(self, items):
        self.items = items

    def __len__(self):
        return len(self.items)

    def __getitem__(self, index):
        return self.items[index]

    def __contains__(self, value):
        return value in self.items


seq = SequenceLike(["a", "b", "c"])
print(seq.__len__())
print(SequenceLike.__len__(seq))
print(seq.__getitem__(0))
print(seq.__getitem__(2))
print(SequenceLike.__getitem__(seq, 1))
print(seq.__contains__("b"))
print(seq.__contains__("z"))

bound_len = seq.__len__
bound_get = seq.__getitem__
print(bound_len())
print(bound_get(1))
print(isinstance(seq, SequenceLike))
