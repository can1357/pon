# Derived from CPython v3.14.0 Lib/test/test_iter.py topics (PSF license).

class Count:
    def __init__(self, limit):
        self.limit = limit
        self.index = 0

    def __iter__(self):
        return self

    def __next__(self):
        value = self.index
        if value >= self.limit:
            raise StopIteration
        self.index = value + 1
        return value


def collect(iterator):
    result = []
    for value in iterator:
        result.append(value)
    else:
        result.append("else")
    return result


def stop_at_two():
    result = []
    for value in Count(5):
        if value == 2:
            result.append("break")
            break
        result.append(value)
    else:
        result.append("else")
    return result


it = Count(3)
print("identity", iter(it) is it, iter(iter(it)) is it)
print("collect", collect(it))
print("exhausted", collect(it))
print("break", stop_at_two())
