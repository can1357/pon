import time

# The conformance runner pins TZ=UTC, so gmtime/localtime tuples and %Z/%z
# are deterministic, and the C locale (the only one pon implements) matches
# the macOS oracle's strftime engine.

DIRECTIVES = [
    '%Y', '%m', '%d', '%H', '%M', '%S', '%j', '%U', '%W', '%w', '%u',
    '%a', '%A', '%b', '%B', '%c', '%x', '%X', '%p', '%I', '%y', '%C',
    '%Z', '%z', '%e', '%F', '%T', '%D', '%h', '%R', '%r', '%k', '%l',
    '%v', '%G', '%g', '%V', '%s', '%+',
]
# The last five stamps sit on ISO-week/year boundaries (Jan 1 on a Monday,
# Dec 31 in ISO week 01 of the next year, Jan 1 in week 53 of the previous
# year, and both ends of year 2000).
STAMPS = [
    0, 1, 59999, 951782400, 987654321, 1234567890, 1699999999, 2524607999,
    1704067200, 1735603200, 1104537600, 946684800, 978220800,
]

for ts in STAMPS:
    t = time.gmtime(ts)
    print(ts, '|'.join(time.strftime(d, t) for d in DIRECTIVES))

# Composite formats (logging's default date format among them) and literal
# text, including the %% escape and non-ASCII pass-through.
t = time.gmtime(1699999999)
print(time.strftime('%Y-%m-%d %H:%M:%S', t))
print(time.strftime('[%a %d %b %Y %H:%M:%S %p %Z]', t))
print(time.strftime('%%Y is %Y, 100%% sure on %A', t))
print(time.strftime('h\u00e9llo %Y \u2713', t))
print(repr(time.strftime('', t)))
print(repr(time.strftime('no directives', t)))
print(repr(time.strftime('%n%t', t)))

# Explicit tuples: CPython's normalization (month 0 -> January, mday 0 -> 1,
# yday 0 -> 1) and the year edges of the engine's _yconv split.
print(time.strftime('%Y|%y|%C|%F|%c|%x', (5, 1, 1, 0, 0, 0, 3, 1, 0)))
print(time.strftime('%Y|%y|%C', (0, 1, 1, 0, 0, 0, 3, 1, 0)))
print(time.strftime('%Y|%y|%C', (12345, 1, 1, 0, 0, 0, 3, 1, 0)))
print(time.strftime('%Y|%y|%C', (-100, 1, 1, 0, 0, 0, 3, 1, 0)))
print(time.strftime('%Y-%m-%d %w %j', (2023, 0, 1, 0, 0, 0, 0, 1, 0)))
print(time.strftime('%Y-%m-%d %w %j', (2023, 1, 0, 0, 0, 0, 7, 0, 0)))
print(time.strftime('%Y-%m-%d %w', (2023, 1, 1, 0, 0, 0, -1, 1, 0)))
print(time.strftime('%m', (2023, True, 1, 0, 0, 0, 0, 1, 0)))

# 12-hour clock across the day, plus the %p boundaries.
for hour in (0, 1, 11, 12, 13, 23):
    print(time.strftime('%H|%I|%l|%k|%p|%r', (2023, 6, 15, hour, 5, 9, 3, 166, 0)))

# Platform pass-through: the macOS engine drops '%' before an unknown
# conversion and preserves a spec cut off by end-of-string.  test.support's
# has_strftime_extensions import-time probe relies on '%4Y'.
print(time.strftime('%4Y', time.gmtime(0)))
print(time.strftime('%4Y') != '%4Y')
WEIRD = [
    '%q', 'x%!y', '%^a', '%#a', '%-4d', '%04d', '%%%%Y', 'abc%', '%',
    '%-', '%_', '%0', '%E', '%O', '%Eq', '%O4', '%_0d', '%--d', '%-%',
    'a%-', '%Ed', '%OY', '%OS', '%GT%g',
]
for weird in WEIRD:
    print(repr(weird), repr(time.strftime(weird, time.gmtime(0))))

# Padding flags on the fixed-width numeric conversions.
t9 = time.gmtime(0)
FLAGGED = [
    '%-d', '%_H', '%0e', '%-j', '%_j', '%0j', '%_m', '%-m', '%-U', '%0U',
    '%-V', '%0V', '%_w', '%0w', '%-t',
]
for f in FLAGGED:
    print(repr(f), repr(time.strftime(f, t9)))

# Range validation: CPython's exact ValueError messages.
for bad in [
    (2023, 13, 1, 0, 0, 0, 0, 1, 0),
    (2023, -1, 1, 0, 0, 0, 0, 1, 0),
    (2023, 1, 32, 0, 0, 0, 0, 1, 0),
    (2023, 1, -1, 0, 0, 0, 0, 1, 0),
    (2023, 1, 1, 24, 0, 0, 0, 1, 0),
    (2023, 1, 1, -1, 0, 0, 0, 1, 0),
    (2023, 1, 1, 0, 60, 0, 0, 1, 0),
    (2023, 1, 1, 0, 0, 62, 0, 1, 0),
    (2023, 1, 1, 0, 0, 0, -2, 1, 0),
    (2023, 1, 1, 0, 0, 0, 0, 367, 0),
    (2023, 1, 1, 0, 0, 0, 0, -1, 0),
]:
    try:
        time.strftime('%c', bad)
        print('no error', bad)
    except ValueError as exc:
        print('ValueError:', exc)

# Argument errors, verbatim CPython messages.
try:
    time.strftime(b'%Y')
except TypeError as exc:
    print('TypeError:', exc)
try:
    time.strftime('%Y', (1, 2, 3))
except TypeError as exc:
    print('TypeError:', exc)
try:
    time.strftime('%Y', [2023, 1, 1, 0, 0, 0, 0, 1, 0])
except TypeError as exc:
    print('TypeError:', exc)
try:
    time.strftime('%Y', ('a', 1, 1, 0, 0, 0, 0, 1, 0))
except TypeError as exc:
    print('TypeError:', exc)
try:
    time.strftime('%Y', (2023.5, 1, 1, 0, 0, 0, 0, 1, 0))
except TypeError as exc:
    print('TypeError:', exc)

# Default argument: the current localtime (UTC here).  Wall-clock values are
# unpredictable, so assert shape invariants only.
out = time.strftime('%Y-%m-%dT%H:%M:%S')
print(len(out) == 19, out[4] == '-', out[13] == ':')
print(int(time.strftime('%Y')) >= 2026)
