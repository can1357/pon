print("rich compare raw results")


# --- comparison EXPRESSIONS pass the dunder result through uncoerced ---


class StrEq:
    def __eq__(self, other):
        return "matched:" + repr(other)

    def __ne__(self, other):
        return ["ne", other]


s = StrEq()
print(s == 1)
print(s == "x")
print(s != 7)
print(repr(s == 1))


class Payload:
    def __init__(self, label):
        self.label = label

    def __repr__(self):
        return "Payload(" + self.label + ")"


class ObjEq:
    def __eq__(self, other):
        return Payload("eq")

    def __lt__(self, other):
        return Payload("lt")

    def __ge__(self, other):
        return 0


o = ObjEq()
print(o == None)
print(o < 99)
print(o >= 99, repr(o >= 99))

# The raw result also flows through returns, containers, and assignments.
r = o < 1
print(type(r).__name__, r)
print([s == 2, o < 3])


def compare_through_call(a, b):
    return a == b


print(compare_through_call(s, 5))
print(compare_through_call(1, 1), compare_through_call(1, 2))

# --- builtins still produce canonical bool singletons ---
print((1 == 1) is True)
print((1 == 2) is False)
print((1 < 2) is True)
print(("a" == "a") is True)
print(("a" < "b") is True)
print(("a" > "b") is False)
print(([1] == [1]) is True)
print(((1, 2) <= (1, 3)) is True)
print(({1} == {1}) is True)
print(({"k": 1} == {"k": 1}) is True)
print((1.5 < 2) is True)
print((1 == 1.0) is True)
print((2.5 == 2.5) is True)
print((1j == 1j) is True)
print((1j == 2j) is False)
print((True == 1) is True)
print((float("nan") == float("nan")) is False)
print((float("nan") != float("nan")) is True)


class Plain:
    pass


p = Plain()
q = Plain()
print((p == p) is True)
print((p == q) is False)
print((p != q) is True)

# --- chained comparisons: intermediates are truth-tested, raw falsy wins ---
log = []


class Chain:
    def __init__(self, label, result):
        self.label = label
        self.result = result

    def __lt__(self, other):
        log.append(self.label + ".lt")
        return self.result


falsy = Chain("falsy", "")
truthy = Chain("truthy", "yes")

log = []
r = falsy < 1 < 99
print(repr(r), log)

log = []
r = truthy < 1 < 99
print(repr(r), log)

log = []
r = truthy < falsy < 99
print(repr(r), log)

# Middle operand evaluated exactly once, left to right.
order = []


def side(label, value):
    order.append(label)
    return value


r = side("a", 1) < side("b", 2) < side("c", 3)
print(r, order)

order = []
r = side("a", 5) < side("b", 2) < side("c", 3)
print(r, order)

# Three-op chain short-circuits at the first falsy intermediate.
log = []
r = falsy < truthy < falsy < truthy
print(repr(r), log)

# Chained comparison over builtins keeps canonical bools.
print((1 < 2 < 3) is True)
print((1 < 2 < 2) is False)

# --- boolean contexts still truth-test the raw result ---
if s == 1:
    print("truthy dunder branch")
if not (o >= 5):
    print("falsy dunder branch")
while falsy < 1:
    print("never")
print("done" if truthy < 1 else "unreachable")
print(bool(s == 1), bool(o >= 5))
print(not (s == 1), not (o >= 5))

# and/or over comparison results keep raw operands (CPython `and` yields
# the deciding operand value).
print((s == 1) and 7)
print((o >= 5) or "fallback")

# is / is not / in / not in stay canonical bools.
print((p is p) is True)
print((p is not p) is False)
print((1 in [1]) is True)
print((1 not in [1]) is False)
