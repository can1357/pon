# type.mro and cls.mro method behavior.


def names(seq):
    return [cls.__name__ for cls in seq]


class Root:
    pass


class Left(Root):
    pass


class Right(Root):
    pass


class Diamond(Left, Right):
    pass


class Shadow:
    mro = 5


cls_mro = Diamond.mro()
unbound_mro = type.mro(Diamond)
int_mro = int.mro()

print("cls mro", names(cls_mro))
print("type mro", names(unbound_mro))
print("int mro", names(int_mro))
print("returns list", type(cls_mro).__name__, isinstance(cls_mro, list), isinstance(cls_mro, tuple))
print("shadow", Shadow.mro)
print("mro tuple", type(Diamond.__mro__).__name__, names(Diamond.__mro__))
print("agreement", tuple(cls_mro) == Diamond.__mro__)
