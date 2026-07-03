print(int.__hash__ is None, type(5).__hash__ is None, str.__hash__ is None)
print(list.__hash__ is None, dict.__hash__ is None, set.__hash__ is None)
print(str.__hash__("ab") == hash("ab"), int.__hash__(5) == 5)

class P:
    pass

print(P.__hash__ is None)

class WithEq:
    def __eq__(self, other):
        return True

print(WithEq.__hash__ is None)

class Sub(dict):
    pass

print(Sub.__hash__ is None)

# dataclasses.py:864 shape: hashability probe of a default's class
# (`.__class__` on builtin instances is a separate closed-getattro gap;
# `type(x)` reaches the same type object)
probes = []
for default in ("s", (1, 2), 5, None):
    probes.append(type(default).__hash__ is None)
print(probes)
print(type([]).__hash__ is None, type({}).__hash__ is None)
