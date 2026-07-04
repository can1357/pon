import logging
import time


class SuperInit:
    def __init__(self):
        super().__init__()
        print("after super")


class SuperSetAttr:
    def write(self):
        super().__setattr__("x", 42)
        return self.x


class SuperEq:
    def eq_self(self):
        return super().__eq__(self)

    def eq_other(self):
        return super().__eq__(object())


class ConverterHolder:
    converter = time.localtime

    def convert(self, value):
        return self.converter(value).tm_year


class Plain:
    pass


SuperInit()
set_attr = SuperSetAttr()
print("setattr", set_attr.write(), set_attr.x)
eq = SuperEq()
print("eq-self", eq.eq_self())
print("eq-other-is-notimplemented", eq.eq_other() is NotImplemented)
plain = Plain()
print("plain-init", plain.__init__() is None)
print("class-init", object.__init__(plain) is None)
print("converter-year", ConverterHolder().convert(0))
record = logging.LogRecord("probe", logging.INFO, "super_bind_probe.py", 10, "hello %s", ("world",), None)
record.created = 946684800.0
record.msecs = 0.0
formatter = logging.Formatter("%(asctime)s %(message)s")
formatted_time = formatter.formatTime(record)
formatted = formatter.format(record)
print("formatTime-smoke", formatted_time.endswith(",000"))
print("format-smoke", formatted.endswith("hello world"))
