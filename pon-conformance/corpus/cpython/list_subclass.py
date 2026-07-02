# List-subclass instances embed native list storage: the full method and
# protocol surface must behave like CPython, and defining a subclass must
# not perturb exact-list construction (regression guard: the type-call
# path must not re-consume one-shot iterables through list.__init__).

class L(list):
    pass

x = L()
x.append(1)
x.append(2)
print(x)
print(len(x))
print(x[0], x[-1])
x.extend([3, 4])
print(x)
print(x.pop())
x[0] = 10
print(x)
print(x == [10, 2, 3])
print([10, 2, 3] == x)
print(2 in x, 99 in x)
del x[0]
x.insert(0, "a")
x.reverse()
print(x)
print(x.index(2))
x.remove("a")
x.sort()
print(x)

# Constructor forms.
print(L("ab"))
print(L([5, 6])[1])
print(L(range(3)))
print(type(L("ab")) is L)
print(isinstance(L(), list))

# Subclass with methods and attributes over the storage.
class Tagged(list):
    def label(self):
        return "n=%d" % len(self)

t = Tagged([7, 8])
print(t.label())
t.append(9)
print(t.label(), t)

class WithAttrs(list):
    def __init__(self, items, name):
        super().__init__(items)
        self.name = name

w = WithAttrs([1, 2], "w1")
print(w, w.name, len(w))

# Slicing, iteration, and consumption by other constructors.
sub = L([1, 2, 3, 4])
print(sub[1:3], type(sub[1:3]) is list)
sub[1:3] = ["x"]
print(sub)
for i, v in enumerate(L(["p", "q"])):
    print(i, v)
print(sorted(L([3, 1, 2])))
print(tuple(L([1, 2])))
print(sum(L([1, 2, 3])))
print(str(x), repr(x))

# REGRESSION GUARD: after a list subclass exists, exact-list construction
# from one-shot iterators must still consume them exactly once.
print(list(map(str, [1, 2, 3])))
print(list(zip([1, 2], "ab")))
print(list(filter(None, [0, 1, 2])))
it = iter([9, 8])
print(list(it))
print(dict([("k", 1)]))
