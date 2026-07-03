try:
    import _frozen_importlib
except ImportError as exc:
    print(type(exc).__name__)
    print(repr(getattr(exc, 'name', None)))
    print(str(exc))
