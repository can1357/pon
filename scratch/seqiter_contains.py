class S:
    def __getitem__(self, i):
        if i > 2:
            raise IndexError
        return i * 10


print(10 in S(), 55 in S())


class OnlyIter:
    def __iter__(self):
        return iter([1, 2])


print(2 in OnlyIter(), 9 in OnlyIter())
try:
    1 in object()
except TypeError as exc:
    print("contains TypeError:", exc)


class BoomEq:
    def __eq__(self, other):
        raise RuntimeError("eq boom")


try:
    BoomEq() in S()
except RuntimeError as exc:
    print("eq error:", exc)
