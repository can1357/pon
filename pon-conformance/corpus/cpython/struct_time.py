import time

# time.struct_time is the structseq returned by gmtime/localtime: a tuple
# subclass with nine named read-only fields.  The conformance runner pins
# TZ=UTC, so every field value is exactly reproducible on both engines.
t = time.gmtime(0)
print(type(t).__name__, type(t) is time.struct_time)
print(repr(t))
print(t.tm_year, t.tm_mon, t.tm_mday, t.tm_hour, t.tm_min, t.tm_sec, t.tm_wday, t.tm_yday, t.tm_isdst)

FIELDS = ('tm_year', 'tm_mon', 'tm_mday', 'tm_hour', 'tm_min', 'tm_sec', 'tm_wday', 'tm_yday', 'tm_isdst')

# Attribute/index parity across a spread of stamps: epoch, a plain modern
# stamp, the 2000-02-29 century leap day, an end-of-century second,
# pre-epoch, and float truncation.
for ts in (0, 1234567890, 951782400, 2524607999, -1, 1.7):
    g = time.gmtime(ts)
    print(ts, [getattr(g, n) for n in FIELDS] == list(g), tuple(g))

# localtime IS gmtime under the pinned TZ=UTC, class identity included.
lt = time.localtime(1234567890)
print(lt == time.gmtime(1234567890), type(lt) is time.struct_time, lt.tm_year, lt.tm_hour)

# Tuple protocol over the subclass layout.
print(len(t), t[0], t[-1], t[2:5], list(t)[:3])
print(t == tuple(t), tuple(t) == t, t == time.gmtime(0), t != time.gmtime(1))
print(isinstance(t, tuple), isinstance(t, time.struct_time))
a, b, c, *rest = t
print(a, b, c, rest)
print(max(t), min(t), sum(t))
print(hash(t) == hash(tuple(t)))
print({t: 'epoch'}[tuple(t)])
print(type(t[0:3]) is tuple, type(tuple(t)) is tuple)

# Comparison is tuple ordering.
print(t < time.gmtime(1), time.gmtime(1) > t, sorted([time.gmtime(86400), t])[0] == t)

# strftime interop: struct_time goes in wherever a 9-tuple does.
print(time.strftime('%Y-%m-%d %H:%M:%S', t))
print(time.strftime('%a %d %b %Y %H:%M:%S %Z', time.gmtime(1699999999)))
print(time.strftime('%j|%U|%W|%w', time.localtime(951782400)))

# Explicit construction from 9-item sequences and iterators.
s = time.struct_time((2009, 2, 13, 23, 31, 30, 4, 44, 0))
print(repr(s))
print(s.tm_year, s[0], s.tm_isdst, s[-1])
print(time.strftime('%Y-%m-%d %H:%M:%S', s))
print(repr(time.struct_time([1999, 12, 31, 23, 59, 59, 4, 365, 0])))
print(time.struct_time(iter((2000, 1, 1, 0, 0, 0, 5, 1, 0))).tm_wday)
print(s == (2009, 2, 13, 23, 31, 30, 4, 44, 0), s == t)

# Fields are read-only; assignment raises AttributeError on both engines
# (messages differ: CPython structseq vs property, so type only).
try:
    t.tm_year = 2000
    print('assigned?!')
except AttributeError:
    print('AttributeError')

# Class-level surface consumed by pydoc/logging paths.
print(time.struct_time.__module__, time.struct_time.__name__)
print(time.gmtime(0).tm_year == 1970)
