# Register-roulette matrix: N fresh-str temps live across a collecting call,
# with same-size-class churn after the collect to stomp freed blocks.
import gc


def mk(tag, n):
    # Fresh, uninterned strings sized into the small size classes.
    return tag + "-" + ("q" * n)


def collect_churn():
    gc.collect()
    junk = []
    for i in range(200):
        junk.append("z" * (4 + (i % 12)))
    gc.collect()
    return len(junk)


def shape1():
    print("s1", mk("a", 4), collect_churn())


def shape2():
    print("s2", mk("a", 4), mk("b", 5), collect_churn())


def shape4():
    print("s4", mk("a", 4), mk("b", 5), mk("c", 6), mk("d", 7), collect_churn())


def shape6():
    print("s6", mk("a", 4), mk("b", 5), mk("c", 6), mk("d", 7), mk("e", 8), mk("f", 9), collect_churn())


def shape8():
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


def shape10():
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
    # Non-str temps: list/tuple/dict displays live across the collect.
    print("cont", [1, 2, 3], ("t", 4), {"k": 5}, collect_churn(), [6, 7])


def nested():
    # Temp live across TWO collecting calls.
    print("nest", mk("n", 6), collect_churn(), collect_churn(), mk("m", 6))


for it in range(5):
    shape1()
    shape2()
    shape4()
    shape6()
    shape8()
    shape10()
    containers()
    nested()
print("matrix-done")
