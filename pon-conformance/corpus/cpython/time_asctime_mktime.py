# Direct time.asctime()/time.mktime() coverage for the env/time near-pass:
# explicit asctime formatting, %c day-spacing parity, and libc-style mktime
# normalization for overflow/underflow month/day/hour legs under TZ=UTC.

import time

for case in (
    time.gmtime(0),
    (5, 1, 1, 0, 0, 0, 3, 1, 0),
    (2000, 2, 29, 0, 0, 0, 1, 60, 0),
):
    print(time.asctime(case))

print(time.strftime('%c', (5, 1, 1, 0, 0, 0, 3, 1, 0)))

for case in (
    time.gmtime(0),
    (2001, 13, 1, 0, 0, 0, 0, 0, 0),
    (2001, 1, 32, 0, 0, 0, 0, 0, 0),
    (2001, 1, 1, 24, 0, 0, 0, 0, 0),
    (2001, 0, 1, 0, 0, 0, 0, 0, 0),
    (2001, 1, 1, -1, 0, 0, 0, 0, 0),
):
    print(time.mktime(case))
