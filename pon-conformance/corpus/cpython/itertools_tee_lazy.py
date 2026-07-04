# itertools.tee laziness, buffering, arity, and errors.
import itertools


def noisy():
    for index in range(4):
        print("pull", index)
        yield index


def show_error(label, operation):
    try:
        operation()
    except Exception as exc:
        print(label, type(exc).__name__, str(exc))


default_a, default_b = itertools.tee(["a", "b"])
print("default", list(default_a), list(default_b))
print("n0", itertools.tee([1, 2], 0))

first, second, third = itertools.tee(noisy(), 3)
print("type", type(first).__name__)
print("first0", next(first))
print("second0", next(second))
print("first1", next(first))
print("third0", next(third))
print("second1", next(second))
print("first tail", list(first))
print("second tail", list(second))
print("third tail", list(third))

count_a, count_b = itertools.tee(itertools.count(10))
print("count a", list(itertools.islice(count_a, 3)))
print("count b", [next(count_b), next(count_b), next(count_b), next(count_b)])

show_error("bad iterable", lambda: itertools.tee(5))
show_error("negative n", lambda: itertools.tee([], -1))
show_error("bad n", lambda: itertools.tee([], "x"))
