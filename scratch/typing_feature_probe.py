import typing
print(typing.__name__)
T = typing.TypeVar('T')
P = typing.ParamSpec('P')
Ts = typing.TypeVarTuple('Ts')
print(repr(T), repr(P), repr(Ts))
class Box(typing.Generic[T]):
    pass
print(Box.__parameters__)
print(typing.Union[int, str].__args__)
print(P.args.__origin__ is P, P.kwargs.__origin__ is P)
print(T.has_default(), typing.TypeVar('TD', default=int).has_default())
