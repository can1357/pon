# Derived from CPython v3.14.0 Lib/test/test_exceptions.py topics (PSF license).
# Attribute surface of BaseException subclass instances: args storage,
# chaining metadata, notes, and instance/class attribute resolution.


class E(ValueError):
    marker = "cls-attr"

    def hint(self):
        return "hint:" + self.args[0]


class Deep(E):
    pass


def constructor_args():
    print("args0", E().args)
    print("args1", E("x").args)
    print("args2", E("x", "y").args)
    print("deep", Deep("d", 1, True).args)
    print("builtin", ValueError("v", 2).args)


def stringify():
    print("str0", repr(str(E())))
    print("str1", str(E("x")))
    print("str2", str(E("x", "y")))


def explicit_cause():
    try:
        raise E("boom") from KeyError("k")
    except E as exc:
        print(
            "cause",
            type(exc.__cause__).__name__,
            exc.__cause__.args,
            exc.__context__,
            exc.__suppress_context__,
        )


def suppressed_context():
    try:
        try:
            raise OSError("hidden")
        except OSError:
            raise E("shown") from None
    except E as exc:
        print(
            "from-none",
            exc.__cause__,
            type(exc.__context__).__name__,
            exc.__suppress_context__,
        )


def implicit_context():
    try:
        try:
            raise KeyError("first")
        except KeyError:
            raise E("second")
    except E as exc:
        print(
            "implicit",
            exc.__cause__,
            type(exc.__context__).__name__,
            exc.__suppress_context__,
        )


def notes():
    e = E("x")
    print("notes-before", hasattr(e, "__notes__"))
    e.add_note("n1")
    e.add_note("n2")
    print("notes-after", e.__notes__)
    try:
        e.add_note(3)
    except TypeError:
        print("notes-typeerror")


def instance_and_class_attrs():
    e = E("x")
    print("resolve", e.marker, e.hint())
    e.code = 42
    print("set-get", e.code)
    e.args = ("a", "b")
    print("args-set", e.args, str(e))


def with_traceback_roundtrip():
    e = E("x")
    print("with-tb", e.with_traceback(None) is e, e.__traceback__)


class Stop(StopIteration):
    pass


def stop_value():
    print("stop-value", Stop("v").value)


class Custom(Exception):
    def __str__(self):
        return "custom:" + repr(self.args)


def custom_str():
    print("custom-str", str(Custom(1, 2)))


constructor_args()
stringify()
explicit_cause()
suppressed_context()
implicit_context()
notes()
instance_and_class_attrs()
with_traceback_roundtrip()
stop_value()
custom_str()
