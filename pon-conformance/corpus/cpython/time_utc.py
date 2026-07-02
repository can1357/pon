import time

# The conformance environment pins TZ=UTC, so localtime IS gmtime and the
# nine struct_time fields are exactly reproducible.
print(tuple(time.gmtime(0)))
print(tuple(time.localtime(0)))
print(tuple(time.gmtime(1234567890)))
print(tuple(time.localtime(1234567890)))
print(tuple(time.gmtime(2524607999)))
print(tuple(time.gmtime(951782400)))  # 2000-02-29: century leap day
print(tuple(time.gmtime(-1)))  # pre-epoch
print(tuple(time.gmtime(1.7)))  # float seconds truncate
print(len(tuple(time.gmtime(0))))

# Wall clock and monotonic families: values are unpredictable, so assert
# types and ordering invariants only.
t = time.time()
tn = time.time_ns()
print(type(t).__name__, type(tn).__name__)
print(t > 1_700_000_000, tn > 1_700_000_000_000_000_000)
m0 = time.monotonic()
p0 = time.perf_counter()
time.sleep(0.01)
m1 = time.monotonic()
p1 = time.perf_counter()
print(m1 > m0, p1 > p0)
print(m1 - m0 >= 0.009, p1 - p0 >= 0.009)
print(type(time.monotonic_ns()).__name__, type(time.perf_counter_ns()).__name__)
