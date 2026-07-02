class Both(OSError, ValueError):
    pass


def raise_both(text):
    raise Both(text)


# Caught through the second declared base, not just the tp_base chain.
try:
    raise_both("second")
except ValueError as exc:
    print("second base:", type(exc).__name__)

# First declared base still matches.
try:
    raise_both("first")
except OSError as exc:
    print("first base:", type(exc).__name__)

# Clause order wins when both clauses name a base of the raised type.
try:
    raise_both("ordered")
except ValueError:
    print("ordered: ValueError clause")
except OSError:
    print("ordered: OSError clause")

# The shared ancestor matches too.
try:
    raise_both("ancestor")
except Exception as exc:
    print("ancestor:", type(exc).__name__)

# Tuple-of-types clause: a non-matching leading element keeps walking.
try:
    raise_both("tuple")
except (KeyError, ValueError) as exc:
    print("tuple second base:", type(exc).__name__)

try:
    raise_both("tuple2")
except (TypeError, OSError) as exc:
    print("tuple first base:", type(exc).__name__)

# A clause naming an unrelated type must not match; the same exception
# object keeps propagating to the outer handler.
try:
    try:
        raise_both("fallthrough")
    except KeyError:
        print("BUG: KeyError caught")
except ValueError as exc:
    print("fallthrough:", type(exc).__name__)

# except* over groups holding a dual-base member: each base clause can claim it.
try:
    raise ExceptionGroup("g1", [Both("m1"), TypeError("t")])
except* ValueError as exc:
    print("star second base:", [type(item).__name__ for item in exc.exceptions])
except* TypeError as exc:
    print("star other:", [type(item).__name__ for item in exc.exceptions])

try:
    raise ExceptionGroup("g2", [Both("m2"), KeyError("k")])
except* OSError as exc:
    print("star first base:", [type(item).__name__ for item in exc.exceptions])
except* KeyError as exc:
    print("star rest:", [type(item).__name__ for item in exc.exceptions])

# isinstance/issubclass agree with the except machinery.
probe = Both("probe")
print("isinstance:", isinstance(probe, ValueError), isinstance(probe, OSError), isinstance(probe, Exception), isinstance(probe, KeyError))
print("issubclass:", issubclass(Both, ValueError), issubclass(Both, OSError), issubclass(Both, (KeyError, ValueError)))
