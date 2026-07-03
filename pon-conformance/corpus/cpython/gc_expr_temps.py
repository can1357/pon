# Expression temporaries across collecting windows: a temp evaluated before
# a call (or dunder dispatch) that reaches gc.collect() is not a named local,
# so only the tier-0 temp-spill windows root it.  Pre-spill, the 8-temp
# ladder shapes below deterministically printed churn bytes in place of a
# freed temp and then crashed (register-parked temp, block reused).  The
# weakref shape guards the other direction: spill slots must hold exactly
# the live temps, never retaining a deleted object.
import gc
import weakref


def mk(tag, n):
    # Fresh, uninterned small strings (same size classes as the churn).
    return tag + "-" + ("q" * n)


def collect_churn():
    gc.collect()
    junk = []
    for i in range(200):
        junk.append("z" * (4 + (i % 12)))
    gc.collect()
    return len(junk)


def ladder1():
    print("s1", mk("a", 4), collect_churn())


def ladder4():
    print("s4", mk("a", 4), mk("b", 5), mk("c", 6), mk("d", 7), collect_churn())


def ladder8():
    print(
        "s8",
        mk("a", 4),
        mk("b", 5),
        mk("c", 6),
        mk("d", 7),
        mk("e", 8),
        mk("f", 9),
        mk("g", 10),
        mk("h", 11),
        collect_churn(),
    )


def ladder10():
    print(
        "s10",
        mk("a", 4),
        mk("b", 5),
        mk("c", 6),
        mk("d", 7),
        mk("e", 8),
        mk("f", 9),
        mk("g", 10),
        mk("h", 11),
        mk("i", 12),
        mk("j", 13),
        collect_churn(),
    )


def containers():
    # Container displays live across the collecting window.
    print("cont", [1, 2, 3], ("t", 4), {"k": 5}, collect_churn(), [6, 7])


def nested_windows():
    # One temp live across TWO collecting windows back to back.
    print("nest", mk("n", 6), collect_churn(), collect_churn(), mk("m", 6))


def method_window():
    class Sink:
        def take(self, a, b, c):
            return a + "|" + str(b) + "|" + c

    sink = Sink()
    # pon_call_method window: temps live across the collecting positional arg.
    print("meth", sink.take(mk("ma", 5), collect_churn(), mk("mb", 6)))


def star_window():
    def spread(*args, **kw):
        return "/".join(str(a) for a in args) + "+" + str(kw.get("k"))

    tail = ("t1", "t2")
    # pon_call_ex window: temp and ** mapping live across the collecting arg.
    print("star", spread(mk("sa", 5), collect_churn(), *tail, k=mk("sk", 4)))


class AddCollects:
    def __add__(self, other):
        gc.collect()
        return "added-" + ("w" * 6)


class EqCollects:
    def __eq__(self, other):
        gc.collect()
        return True


class BoolCollects:
    def __bool__(self):
        gc.collect()
        return True


class ItemCollects:
    def __getitem__(self, key):
        gc.collect()
        return "item-" + str(key)


class PropCollects:
    @property
    def prop(self):
        gc.collect()
        return "prop-" + ("v" * 5)


def dunder_windows():
    # Temps live across dunder dispatch that collects: BinaryOp, Compare,
    # truth-test branch, SubscriptGet, and LoadAttr(property) windows.
    print("add", mk("da", 6), AddCollects() + 0, mk("db", 7))
    print("eq", mk("ea", 6), EqCollects() == 1, mk("eb", 7))
    print("bool", mk("ba", 6), "yes" if BoolCollects() else "no", mk("bb", 7))
    print("item", mk("ia", 6), ItemCollects()[3], mk("ib", 7))
    print("prop", mk("pa", 6), PropCollects().prop, mk("pb", 7))


def fstring_window():
    aux = mk("fa", 5)
    print("fstr", aux + "!", f"pre-{collect_churn()}-post", aux + "?")


def gen_window():
    def gen(tag):
        yield (tag + "-one", collect_churn())
        yield (tag + "-two", collect_churn())

    for part in gen("gw"):
        print("gen", part[0], part[1])


class ExitTracked:
    def __init__(self):
        self.state = "open"

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc, tb):
        gc.collect()
        self.state = "closed"
        return False


def with_window():
    cm = ExitTracked()
    with cm:
        print("with-body", mk("wa", 5), collect_churn())
    print("with-exit", cm.state)


class WeakTarget:
    pass


def weakref_not_retained():
    # The spill pool must root exactly the live temps: a deleted local whose
    # object was only ever a consumed temp must still be collected while
    # OTHER temps are parked in the pool across the collecting window.
    target = WeakTarget()
    ref = weakref.ref(target)
    del target
    print("weak-pad", mk("za", 6), collect_churn(), mk("zb", 7))
    print("weak-cleared", ref() is None)


for _round in range(3):
    ladder1()
    ladder4()
    ladder8()
    ladder10()
    containers()
    nested_windows()
    method_window()
    star_window()
    dunder_windows()
    fstring_window()
    gen_window()
    with_window()
weakref_not_retained()
print("gc-expr-temps-done")
