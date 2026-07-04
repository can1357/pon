import typing
print(typing._make_nmtuple("N", ["a"], typing._make_eager_annotate({"a": int}), __name__))
