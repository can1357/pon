# Derived from CPython v3.14.0 Lib/test/test_exceptions.py topics (PSF license).

def implicit_context():
    try:
        raise ValueError("primary")
    except ValueError as first:
        try:
            raise TypeError("secondary")
        except TypeError as second:
            print(
                "implicit",
                type(second).__name__,
                type(second.__context__).__name__,
                second.__context__ is first,
                second.__cause__ is None,
            )


def explicit_cause():
    try:
        try:
            raise KeyError("missing")
        except KeyError as original:
            raise RuntimeError("wrapped") from original
    except RuntimeError as exc:
        print(
            "explicit",
            type(exc.__cause__).__name__,
            type(exc.__context__).__name__,
            exc.__cause__ is exc.__context__,
            exc.__suppress_context__,
        )


def suppressed_context():
    try:
        try:
            raise OSError("hidden")
        except OSError:
            raise LookupError("shown") from None
    except LookupError as exc:
        print(
            "suppressed",
            exc.__cause__ is None,
            type(exc.__context__).__name__,
            exc.__suppress_context__,
        )


implicit_context()
explicit_cause()
suppressed_context()
