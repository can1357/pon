# sys.getrecursionlimit / sys.setrecursionlimit: default value, stored-limit
# round-trip, and the CPython positive-int ValueError leg.  Every accepted
# value stays far above the oracle's live recursion depth so host CPython
# never trips its "limit is too low" RecursionError guard.
import sys

print(sys.getrecursionlimit())

sys.setrecursionlimit(1200)
print(sys.getrecursionlimit())
sys.setrecursionlimit(9000)
print(sys.getrecursionlimit())

for bad in (0, -1, -1000):
    try:
        sys.setrecursionlimit(bad)
        print("unexpectedly accepted", bad)
    except ValueError as exc:
        print("ValueError:", exc)
    print(sys.getrecursionlimit())

print(sys.setrecursionlimit(1000))
print(sys.getrecursionlimit())
