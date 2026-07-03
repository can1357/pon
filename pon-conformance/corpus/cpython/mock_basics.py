# unittest.mock fundamentals over the live instance-__dict__ view and the
# instance-callee keyword-call path: Mock construction writes attributes
# through `self.__dict__[...] = ...` (view write-through), calling a mock
# routes keyword arguments through `type(m).__call__` descriptor dispatch,
# and dir(NonCallableMock) exercises type-based `__dir__` lookup.
from unittest.mock import ANY, DEFAULT, MagicMock, Mock, NonCallableMock, call, sentinel

# Attribute auto-creation and call recording (kwargs through instance call).
m = Mock()
m.foo.bar(1, 2, key="v")
print(m.foo.bar.called)
print(m.foo.bar.call_count)
print(m.foo.bar.call_args == call(1, 2, key="v"))

# return_value / side_effect.
m2 = Mock(return_value=42)
print(m2())
m3 = Mock(side_effect=[1, 2])
print(m3(), m3())

# Assertion helpers.
m4 = Mock()
m4(10)
m4.assert_called_once_with(10)
m4.assert_called_with(10)
print("assert-ok")

# Instance __dict__ surface mock leans on: get/contains/iteration/equality.
print(m4.__dict__["_mock_call_count"])
print("_mock_name" in m4.__dict__)
print(m4.__dict__.get("_mock_absent", "fallback"))

# MagicMock magic-method wiring.
mm = MagicMock()
mm.__len__.return_value = 7
print(len(mm))
print(bool(MagicMock()))

# sentinel identity and ANY equality.
print(sentinel.thing is sentinel.thing)
print(ANY == 12345)
print(DEFAULT is sentinel.DEFAULT)

# dir() filtering path (NonCallableMock defines instance __dir__; the class
# object itself must enumerate via type.__dir__).
names = dir(NonCallableMock)
print("assert_called_with" in names)
inst = NonCallableMock()
print("assert_called_with" in dir(inst))

# reset_mock round trip.
m5 = Mock()
m5(1)
m5.reset_mock()
print(m5.called, m5.call_count)
