try:
    try:
        raise KeyError("missing")
    except KeyError as exc:
        raise RuntimeError("wrapped") from exc
except RuntimeError as exc:
    print(type(exc).__name__, type(exc.__cause__).__name__)
    print(str(exc), str(exc.__cause__))
