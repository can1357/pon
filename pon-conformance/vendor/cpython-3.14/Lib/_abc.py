"""Pure Python implementation of CPython's private _abc accelerator API."""


def get_cache_token():
    return get_cache_token.counter


get_cache_token.counter = 0


def _abc_init(cls):
    abstracts = set()
    for name, value in cls.__dict__.items():
        if getattr(value, "__isabstractmethod__", False):
            abstracts.add(name)
    for base in cls.__bases__:
        for name in getattr(base, "__abstractmethods__", set()):
            value = getattr(cls, name, None)
            if getattr(value, "__isabstractmethod__", False):
                abstracts.add(name)
    cls.__abstractmethods__ = frozenset(abstracts)
    cls._abc_registry = set()
    cls._abc_cache = set()
    cls._abc_negative_cache = set()
    cls._abc_negative_cache_version = get_cache_token.counter


def _direct_subclass(subclass, cls):
    return cls in getattr(subclass, "__mro__", ())


def _abc_register(cls, subclass, _direct_subclass=_direct_subclass):
    if not isinstance(subclass, type):
        raise TypeError("Can only register classes")
    if _abc_subclasscheck(cls, subclass):
        return subclass
    if _direct_subclass(cls, subclass):
        raise RuntimeError("Refusing to create an inheritance cycle")
    cls._abc_registry.add(subclass)
    get_cache_token.counter += 1
    return subclass


def _abc_instancecheck(cls, instance):
    subclass = instance.__class__
    if subclass in cls._abc_cache:
        return True
    subtype = type(instance)
    if subtype is subclass:
        if (cls._abc_negative_cache_version == get_cache_token.counter and
                subclass in cls._abc_negative_cache):
            return False
        return _abc_subclasscheck(cls, subclass)
    return _abc_subclasscheck(cls, subclass) or _abc_subclasscheck(cls, subtype)


def _abc_subclasscheck(cls, subclass, _direct_subclass=_direct_subclass):
    if not isinstance(subclass, type):
        raise TypeError("issubclass() arg 1 must be a class")
    if not hasattr(cls, "_abc_cache"):
        return _direct_subclass(subclass, cls)
    if subclass in cls._abc_cache:
        return True
    if cls._abc_negative_cache_version < get_cache_token.counter:
        cls._abc_negative_cache = set()
        cls._abc_negative_cache_version = get_cache_token.counter
    elif subclass in cls._abc_negative_cache:
        return False

    ok = cls.__subclasshook__(subclass)
    if ok is not NotImplemented:
        if ok:
            cls._abc_cache.add(subclass)
        else:
            cls._abc_negative_cache.add(subclass)
        return ok

    if _direct_subclass(subclass, cls):
        cls._abc_cache.add(subclass)
        return True

    for registered in cls._abc_registry:
        if _direct_subclass(subclass, registered):
            cls._abc_cache.add(subclass)
            return True
        if hasattr(registered, "_abc_cache") and _abc_subclasscheck(registered, subclass):
            cls._abc_cache.add(subclass)
            return True

    for child in cls.__subclasses__():
        if _direct_subclass(subclass, child):
            cls._abc_cache.add(subclass)
            return True
        if hasattr(child, "_abc_cache") and _abc_subclasscheck(child, subclass):
            cls._abc_cache.add(subclass)
            return True

    cls._abc_negative_cache.add(subclass)
    return False


def _get_dump(cls):
    if not hasattr(cls, "_abc_cache"):
        _abc_init(cls)
    return (set(cls._abc_registry), set(cls._abc_cache),
            set(cls._abc_negative_cache), cls._abc_negative_cache_version)


def _reset_registry(cls):
    if not hasattr(cls, "_abc_registry"):
        _abc_init(cls)
    cls._abc_registry.clear()


def _reset_caches(cls):
    if not hasattr(cls, "_abc_cache"):
        _abc_init(cls)
    cls._abc_cache.clear()
    cls._abc_negative_cache.clear()


del _direct_subclass
