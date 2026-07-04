import typing
print("before")
class Outer:
    print("in outer")
    ann = {"a": int}
    N = typing._make_nmtuple("N", ["a"], typing._make_eager_annotate(ann), __name__)
print("after", Outer.N)
