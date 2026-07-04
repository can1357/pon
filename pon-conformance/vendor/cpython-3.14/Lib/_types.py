"""Source-compatible fallback for CPython's private _types module."""

import sys as _sys


def _f():
    pass

FunctionType = type(_f)
LambdaType = type(lambda: None)
CodeType = type(_f.__code__)
MappingProxyType = type(type.__dict__)
SimpleNamespace = type(_sys.implementation)


def _cell_factory():
    a = 1
    def f():
        nonlocal a
    return f.__closure__[0]

CellType = type(_cell_factory())


def _g():
    yield 1

GeneratorType = type(_g())


async def _c():
    pass

_coro = _c()
CoroutineType = type(_coro)
_coro.close()


async def _ag():
    yield

_agen = _ag()
AsyncGeneratorType = type(_agen)


class _C:
    def _m(self):
        pass

MethodType = type(_C()._m)
BuiltinFunctionType = type(len)
BuiltinMethodType = BuiltinFunctionType
WrapperDescriptorType = type(object.__init__)
MethodWrapperType = type(object().__str__)
MethodDescriptorType = type(str.join)
ClassMethodDescriptorType = type(dict.__dict__['fromkeys'])
ModuleType = type(_sys)

try:
    raise TypeError
except TypeError as exc:
    TracebackType = type(exc.__traceback__)
    FrameType = type(exc.__traceback__.tb_frame)

GetSetDescriptorType = type(FunctionType.__code__)
MemberDescriptorType = type(FunctionType.__globals__)
GenericAlias = type(list[int])
UnionType = type(int | str)
EllipsisType = type(Ellipsis)
NoneType = type(None)
NotImplementedType = type(NotImplemented)
class CapsuleType:
    def __new__(cls, *args, **kwargs):
        raise TypeError("cannot create 'PyCapsule' instances")

CapsuleType.__name__ = 'PyCapsule'
CapsuleType.__qualname__ = 'PyCapsule'
CapsuleType.__module__ = 'builtins'

del _sys, _f, _cell_factory, _g, _c, _coro, _ag, _agen, _C
