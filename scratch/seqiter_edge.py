class S:
    def __getitem__(self, i):
        if i > 2:
            raise IndexError
        return i * 10


it = iter(S())
print(type(it).__name__, iter(it) is it)
print(list(S()), tuple(S()))
print(next(it), next(it), next(it))
try:
    next(it)
except StopIteration:
    print("exhausted")
try:
    next(it)
except StopIteration:
    print("still exhausted")


class Stopper:
    def __getitem__(self, i):
        if i == 1:
            raise StopIteration("boom")
        return i


print("stopper", list(Stopper()))


class Boom:
    def __getitem__(self, i):
        if i == 1:
            raise ValueError("kapow")
        return i


try:
    list(Boom())
except ValueError as exc:
    print("boom", exc)


class Sub(S):
    pass


print("inherited", list(Sub()))

try:
    iter(object())
except TypeError as exc:
    print("object TypeError", exc)
try:
    iter(3)
except TypeError as exc:
    print("int TypeError", exc)
for x in S():
    print("for", x)
