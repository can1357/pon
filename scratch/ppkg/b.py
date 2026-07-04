from . import PurePath, from_func

def direct():
    return PurePath('direct').parents

def nested():
    def inner():
        return PurePath('nested').parents
    return inner()

def comp():
    return [p.parents for p in [PurePath('comp')]][0]

def via_func():
    return from_func()
