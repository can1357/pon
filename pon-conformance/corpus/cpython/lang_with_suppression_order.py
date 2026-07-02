# Derived from CPython v3.14.0 Lib/test/test_with.py topics (PSF license).

events = []


class Manager:
    def __init__(self, name, swallow=False, fail_enter=False, fail_exit=False):
        self.name = name
        self.swallow = swallow
        self.fail_enter = fail_enter
        self.fail_exit = fail_exit

    def __enter__(self):
        events.append("enter " + self.name)
        if self.fail_enter:
            raise RuntimeError("enter " + self.name)
        return self

    def __exit__(self, exc_type, exc, traceback):
        if exc_type is None:
            kind = "none"
        else:
            kind = exc_type.__name__
        events.append("exit " + self.name + " " + kind)
        if self.fail_exit:
            raise LookupError("exit " + self.name)
        return self.swallow


try:
    with Manager("outer") as outer, Manager("inner", swallow=True) as inner:
        events.append("body")
        raise ValueError("hidden")
    events.append("after suppressed")
except Exception as exc:
    events.append("unexpected " + type(exc).__name__)
print("suppressed", events)

events = []
try:
    with Manager("first") as first, Manager("second", fail_enter=True):
        events.append("body")
except RuntimeError as exc:
    events.append("caught " + type(exc).__name__)
print("enter-fail", events)

events = []
try:
    with Manager("clean") as clean, Manager("bad-exit", fail_exit=True):
        events.append("body")
except LookupError as exc:
    events.append("caught " + type(exc).__name__)
print("exit-fail", events)
