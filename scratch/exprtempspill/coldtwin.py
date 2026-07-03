# Cold-twin forcing gate (GcWindows): tier the function up via call hotness,
# then deopt it through the region entry guard with a non-int bound whose
# reflected __gt__ collects.  The loop body keeps a fresh concat temp live
# across a periodically-collecting call, so the temp-spill windows are
# exercised in the primary boxed escape (warm rounds) AND the cold twin
# (deopt rounds).
import gc


class GtCollects:
    def __init__(self, limit):
        self.limit = limit

    def __gt__(self, other):
        gc.collect()
        return self.limit > other


def stepper(i):
    if i % 37 == 0:
        gc.collect()
        gc.collect()
    return i


def hot(n, tag):
    total = 0
    i = 0
    marker = "m-init"
    while i < n:
        total = total + i
        marker = ("mk-" + tag) + str(stepper(i))
        i = i + 1
    return total, marker


out = None
for r in range(60):
    out = hot(120, "warm" + str(r))
print("tiered", out[0], out[1])

cold = None
for r in range(6):
    cold = hot(GtCollects(60), "cold" + str(r))
print("cold", cold[0], cold[1])
print("coldtwin-done")
