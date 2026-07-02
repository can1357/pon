# Native `_signal` registration bookkeeping (signal.signal/getsignal
# round-trip through the process handler table, SIG_DFL/SIG_IGN sentinel
# pass-through, default_int_handler raising when CALLED) and the
# sys.exception()/sys.exc_info() handled-exception surface.  Every try/except
# probe lives in its own function: pon resets the handled exception when the
# catching frame returns, so module-level reads stay CPython-clean.
import signal
import sys

# Constants and the IntEnum conversion surface (values host-stable across
# macOS and Linux for these two signals).
print(int(signal.SIGINT), int(signal.SIGTERM))
print(signal.Signals(2).name)
print(signal.Handlers(0).name, signal.Handlers(1).name)
print(signal.getsignal(signal.SIGINT) is signal.default_int_handler)


def handler(signum, frame):
    return None


# signal()/getsignal() round-trip: Python handler in, sentinels back out.
prev = signal.signal(signal.SIGTERM, handler)
print(type(prev).__name__, int(prev))
print(signal.getsignal(signal.SIGTERM) is handler)
back = signal.signal(signal.SIGTERM, signal.SIG_IGN)
print(back is handler)
now = signal.getsignal(signal.SIGTERM)
print(type(now).__name__, int(now))
restored = signal.signal(signal.SIGTERM, signal.SIG_DFL)
print(type(restored).__name__, int(restored))
print(int(signal.getsignal(signal.SIGTERM)))


def probe_range_error():
    try:
        signal.getsignal(0)
    except ValueError as exc:
        return "ValueError: " + str(exc)
    return "no error"


def probe_int_handler_error():
    try:
        signal.signal(signal.SIGTERM, 5)
    except TypeError as exc:
        return "TypeError: " + str(exc)
    return "no error"


def probe_none_handler_error():
    try:
        signal.signal(signal.SIGTERM, None)
    except TypeError as exc:
        return "TypeError: " + str(exc)
    return "no error"


def probe_default_int_handler():
    try:
        signal.default_int_handler(int(signal.SIGINT), None)
    except KeyboardInterrupt:
        return "KeyboardInterrupt raised"
    return "no error"


# Validation errors keep CPython's wording.
print(probe_range_error())
print(probe_int_handler_error())
print(probe_none_handler_error())
print(probe_default_int_handler())

# sys.exception()/sys.exc_info() outside any handler.
print(sys.exception())
print(sys.exc_info())


def catch_and_probe():
    try:
        raise ValueError("boom")
    except ValueError as caught:
        current = sys.exception()
        exc_type, exc_value, exc_tb = sys.exc_info()
        return (
            current is caught,
            exc_type is ValueError,
            exc_value is caught,
            exc_tb is caught.__traceback__,
            exc_tb is not None,
            type(exc_value).__name__,
            exc_value.args,
        )


print(catch_and_probe())
print(sys.exception() is None)
print(sys.exc_info())


def helper_sees_handled():
    return sys.exception()


def catch_with_helper():
    try:
        raise KeyError("k")
    except KeyError as caught:
        return helper_sees_handled() is caught


print(catch_with_helper())
print(sys.exception() is None)
