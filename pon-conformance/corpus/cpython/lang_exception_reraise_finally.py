# Derived from CPython v3.14.0 Lib/test/test_exceptions.py topics (PSF license).

def bare_reraise_same_object():
    saved = []
    try:
        try:
            raise KeyError("token")
        except KeyError as exc:
            saved.append(exc)
            raise
    except KeyError as exc:
        print("reraise", exc is saved[0], exc.args[0])


def bare_raise_without_active_exception():
    try:
        raise
    except RuntimeError as exc:
        print("inactive", type(exc).__name__)


def finally_replaces_try_exception():
    try:
        try:
            raise TypeError("from-try")
        finally:
            raise ValueError("from-finally")
    except ValueError as exc:
        print(
            "finally context",
            type(exc).__name__,
            exc.args[0],
            type(exc.__context__).__name__,
            exc.__context__.args[0],
        )


def except_finally_context_chain():
    try:
        try:
            raise TypeError("try")
        except TypeError:
            raise ValueError("except")
        finally:
            raise OSError("finally")
    except OSError as exc:
        print("chain", type(exc.__context__).__name__, type(exc.__context__.__context__).__name__)


bare_reraise_same_object()
bare_raise_without_active_exception()
finally_replaces_try_exception()
except_finally_context_chain()
