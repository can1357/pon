from unittest.mock import Mock, MagicMock, call, sentinel

m = Mock()
m.foo(k=1)
m.foo.assert_called_once_with(k=1)
print("assert_called_once_with ok")

m2 = Mock(return_value=42)
print("return_value:", m2())
m2.assert_called_once_with()

m3 = Mock()
m3.bar(1, 2, x="y")
print("call_args:", m3.bar.call_args)
print("call_count:", m3.bar.call_count)
try:
    m3.bar.assert_called_once_with(1, 2, x="z")
    print("BUG: no AssertionError")
except AssertionError as e:
    print("mismatch raises AssertionError:", type(e).__name__)

mm = MagicMock()
mm.child.method("a")
print("mock_calls:", mm.mock_calls)
print("call obj:", call.child.method("a"))
print("sentinel:", sentinel.DEFAULT is sentinel.DEFAULT)
