# Derived from CPython v3.14.0 Lib/test/test_grammar.py topics (PSF license).

def break_runs_finally():
    events = []
    for value in [0, 1, 2]:
        try:
            events.append("try-" + str(value))
            if value == 1:
                break
        finally:
            events.append("finally-" + str(value))
    else:
        events.append("for-else")
    events.append("after-break")
    return events


def continue_runs_finally():
    events = []
    count = 0
    while count < 3:
        try:
            events.append("try-" + str(count))
            count += 1
            if count < 3:
                continue
            events.append("last")
        finally:
            events.append("finally-" + str(count))
    else:
        events.append("while-else")
    return events


def return_runs_finally(flag):
    events = []

    def inner():
        try:
            events.append("returning")
            if flag:
                return "true-value"
            return "false-value"
        finally:
            events.append("finally")

    return inner(), events


def exception_runs_finally():
    events = []
    try:
        try:
            events.append("try")
            raise ValueError("raised")
        finally:
            events.append("finally")
    except ValueError as exc:
        events.append(type(exc).__name__)
    return events


def nested_break_order():
    events = []
    for outer in [0, 1]:
        for inner in [0, 1]:
            try:
                events.append("body-" + str(outer) + "-" + str(inner))
                break
            finally:
                events.append("finally-" + str(outer) + "-" + str(inner))
        else:
            events.append("inner-else")
            continue
        events.append("after-inner-" + str(outer))
    else:
        events.append("outer-else")
    return events


print("break", break_runs_finally())
print("continue", continue_runs_finally())
print("return", return_runs_finally(True), return_runs_finally(False))
print("exception", exception_runs_finally())
print("nested", nested_break_order())
