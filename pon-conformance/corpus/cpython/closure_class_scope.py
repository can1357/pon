# Class scopes threading enclosing-function cells to methods and nested
# scopes (functools.cmp_to_key shape and friends).

# 1. cmp_to_key shape: methods capture the enclosing function's parameter.
def cmp_to_key(mycmp):
    class K:
        def __init__(self, obj):
            self.obj = obj
        def __lt__(self, other):
            return mycmp(self.obj, other.obj) < 0
        def __eq__(self, other):
            return mycmp(self.obj, other.obj) == 0
    return K

def reverse_cmp(a, b):
    return (b > a) - (b < a)

Key = cmp_to_key(reverse_cmp)
print("cmp_to_key lt", Key(1) < Key(2), Key(2) < Key(1))
print("cmp_to_key eq", Key(3) == Key(3), Key(3) == Key(4))
print("cmp_to_key sorted", sorted([3, 1, 2], key=cmp_to_key(reverse_cmp)))

# 2. Multiple captured variables across __init__ and regular methods.
def make_scaler(factor, offset):
    label = "scaled"
    class Scaler:
        def __init__(self, value):
            self.value = value * factor
        def shifted(self):
            return self.value + offset
        def describe(self):
            return label + ":" + str(self.value)
    return Scaler

S = make_scaler(10, 3)
inst = S(7)
print("multi", inst.value, inst.shifted(), inst.describe())

# 3. Late binding: methods observe cell updates made after class creation.
def make_counter():
    count = 0
    class Counter:
        def read(self):
            return count
    def bump():
        nonlocal count
        count += 1
    return Counter(), bump

counter, bump = make_counter()
print("late before", counter.read())
bump()
bump()
print("late after", counter.read())

# 4. Two functions + class: capture threads through a middle function that
#    has both its own cell variable and a free variable.
def outer(a):
    def make(b):
        marker = "mid"
        class Pair:
            def values(self):
                return (a, b, marker)
        def touch():
            return marker
        touch()
        return Pair
    return make

Pair = outer("A")("B")
print("nested", Pair().values())

# 5. Direct class-body reads of the enclosing function's local.
def direct(p):
    class C:
        doubled = p * 2
        halved = p // 2
    return C

D = direct(42)
print("direct", D.doubled, D.halved)

# 6. Comprehensions and genexps in the class body capture the function local.
def comp_capture(base):
    class Table:
        squares = [base + i * i for i in range(4)]
        pairs = {i: base + i for i in range(3)}
        lazy = tuple(base * i for i in range(3))
    return Table

T = comp_capture(100)
print("comp", T.squares, sorted(T.pairs.items()), T.lazy)

# 7. Class-local shadow does not satisfy a method capture.
def shadow(q):
    class D:
        q = 77
        def m(self):
            return q
    return D

Dq = shadow(3)
print("shadow", Dq().m(), Dq.q)
