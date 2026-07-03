# ExprTempSpill deterministic repro: expression temporaries live across a
# collecting call are register-parked and invisible to the conservative
# stack scan (pon-gc scans frame memory, never registers).
import gc


def collect_and_churn():
    gc.collect()
    junk = []
    for i in range(64):
        junk.append(("x" * 9) + str(i))
    gc.collect()
    return len(junk)


def label_across_call():
    # "lab" + "el-one" builds a fresh str temp, live across the collecting
    # call until print consumes it.
    prefix = "lab"
    print(prefix + "el-one", collect_and_churn())


def tuple_across_call():
    n = 3
    print(("t-" + str(n), n), collect_and_churn())


def many_temps_across_call():
    a = "a" * 5
    b = "b" * 5
    print(a + "1", b + "2", collect_and_churn(), a + b)


label_across_call()
tuple_across_call()
many_temps_across_call()
print("done")
