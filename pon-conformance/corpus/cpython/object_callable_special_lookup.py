# Derived from CPython v3.14.0 Lib/test/test_descr.py topics (PSF license).

def add_one(value):
    return value + 1


class Worker:
    def __init__(self, offset):
        self.offset = offset

    def apply(self, value):
        return self.offset + value

    def pair(self, left, right):
        return self.offset + left + right


worker = Worker(10)
bound_apply = worker.apply
bound_pair = worker.pair
callbacks = [add_one, bound_apply, bound_pair]

print(callable(add_one))
print(callable(bound_apply))
print(callbacks[0](4))
print(callbacks[1](4))
print(callbacks[2](4, 5))

chosen = callbacks[1]
print(chosen(7))
print(Worker.apply(worker, 8))
