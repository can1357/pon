# callable() must report True for classes regardless of metaclass:
# every metaclass inherits type.tp_call (CPython PyCallable_Check).
import abc
import functools


class Meta(type):
    pass


class WithMeta(metaclass=Meta):
    pass


class Abstract(abc.ABC):
    @abc.abstractmethod
    def f(self): ...


class Concrete(Abstract):
    def f(self):
        return "concrete"


class WithCall:
    def __call__(self):
        return "instance-call"


print(callable(WithMeta))
print(callable(Abstract))
print(callable(Concrete))
print(callable(WithCall))
print(callable(WithCall()))
print(callable(Meta))
print(callable(3))
print(callable(None))

# functools.partial rejects non-callables via callable(); a metaclass
# instance must construct (meson DependencyFactory pattern).
make = functools.partial(Concrete)
print(make().f())
try:
    functools.partial(42)
except TypeError as exc:
    print("TypeError:", exc)

# iter(callable, sentinel) uses the same predicate.
counter = {"n": 0}


class Step(metaclass=Meta):
    def __init__(self):
        counter["n"] += 1
        self.n = counter["n"]

    def __repr__(self):
        return f"Step({self.n})"


it = iter(Step, None)
print(next(it))
print(next(it))
