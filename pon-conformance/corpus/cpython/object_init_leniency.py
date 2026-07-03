# CPython object.__init__ excess-argument rule (Objects/typeobject.c
# object_init): excess arguments reaching the DEFAULT __init__ are tolerated
# exactly when the class overrides __new__ — including a __new__ INHERITED
# from a builtin base's tp_new slot (CPython copies slots at type-ready
# time, so a bytes/tuple subclass carries bytes_new/tuple_new, never
# object_new).  The stdlib gate is multiprocessing.process:
# `AuthenticationString(os.urandom(32))` with AuthenticationString(bytes).

# --- builtin-base subclasses: ctor args tolerated by default __init__ --------
class AS(bytes):
    def __reduce__(self):
        return None

a = AS(b"abcd")
print(type(a).__name__)


class T(tuple):
    pass

t = T((1, 2))
print(type(t).__name__, len(t))


class I2(int):
    pass

print(I2(5) + 1)


class S(str):
    pass

print(S("hi") + "!")


class D(dict):
    pass

d = D({"a": 1})
print(type(d).__name__, d["a"])


class FL(float):
    pass

print(type(FL(1.5)).__name__)


# --- Python-level __new__ override, default __init__: args tolerated ---------
class N:
    def __new__(cls, *args):
        return super().__new__(cls)

print(type(N(1, 2)).__name__)


# --- both __init__ and __new__ at object defaults + args: still an error -----
class P:
    pass

try:
    P(1)
    print("no error")
except TypeError:
    print("TypeError")


# --- the multiprocessing._MainProcess shape ----------------------------------
class MP:
    def __init__(self):
        self._authkey = AS(b"0123456789abcdef")

print(type(MP()._authkey).__name__)
