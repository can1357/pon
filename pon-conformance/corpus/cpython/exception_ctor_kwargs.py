# Derived from CPython v3.14.0 Lib/test/test_exceptions.py topics (PSF license).
# Keyword arguments through exception construction: the ImportError family
# binds name=/path=/name_from= (readable attrs, None defaults), every other
# builtin init rejects keywords with a typed, catchable TypeError, and a
# constructor failure propagates from a raise site instead of morphing into
# "exceptions must derive from BaseException".


def import_error_roundtrip():
    e = ImportError("m", name="n", path="p")
    print("ie", e.name, e.path, e.args, str(e))
    bare = ImportError("m")
    print("ie-defaults", bare.name, bare.path, bare.args)
    kw_only = ImportError(name="only")
    print("ie-kw-only", kw_only.name, kw_only.args, repr(str(kw_only)))
    multi = ImportError("a", "b", name="n")
    print("ie-multi", multi.args, multi.name, str(multi))
    named_from = ImportError("m", name_from="f")
    print("ie-name-from", named_from.name_from, named_from.name)
    assigned = ImportError("m")
    assigned.name = "assigned"
    print("ie-assign", assigned.name)


def module_not_found_subclass():
    e = ModuleNotFoundError("mm", name="nn")
    print("mnfe", type(e).__name__, e.name, e.path, isinstance(e, ImportError))

    class Sub(ModuleNotFoundError):
        pass

    s = Sub("x", name="sn", path="sp")
    print("mnfe-sub", s.name, s.path, s.args)


def plain_exception_kwargs():
    for cls in (Exception, ValueError, KeyboardInterrupt):
        try:
            cls(x=1)
        except TypeError as e:
            print("reject", cls.__name__, e)

    class Plain(Exception):
        pass

    try:
        Plain(x=1)
    except TypeError as e:
        print("reject-subclass", e)


def invalid_import_error_kwarg():
    try:
        ImportError("m", bogus=1)
    except TypeError as e:
        print("ie-bogus", e)
    try:
        ModuleNotFoundError("q", nope=3)
    except TypeError as e:
        print("mnfe-bogus", e)


def raise_from_ctor_error():
    # The constructor failure must reach the handler as the typed TypeError,
    # not be masked by the raise site's derive-from-BaseException check.
    try:
        raise ModuleNotFoundError("q", nope=3)
    except TypeError as e:
        print("raise-ctor-bogus", e)
    try:
        raise Exception(x=1)
    except TypeError as e:
        print("raise-ctor-reject", e)
    try:
        raise ModuleNotFoundError("halted; None in sys.modules", name="pkg.mod")
    except ImportError as e:
        print("raise-ctor-ok", type(e).__name__, e.name, str(e))


def user_init_kwargs():
    # BaseException.__new__ already stored the positional args, so the init
    # only binds the keyword; super().__init__ on exception MROs is a
    # separate pon lane (no __init__ entry on the builtin exception types).
    class WithInit(Exception):
        def __init__(self, a, b=None):
            self.b = b

    w = WithInit("x", b=3)
    print("user-init", w.args, w.b)
    try:
        WithInit("x", c=1)
    except TypeError as e:
        print("user-init-bad", type(e).__name__)


import_error_roundtrip()
module_not_found_subclass()
plain_exception_kwargs()
invalid_import_error_kwarg()
raise_from_ctor_error()
user_init_kwargs()
