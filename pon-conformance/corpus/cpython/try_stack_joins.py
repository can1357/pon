# Handler-stack reconciliation at CFG joins: break/continue/return departing
# a try phase from NESTED statements must pop the phase's handler record and
# run finally bodies on the way out (pin J0.6 §3.1 exit-edge discipline).
print("try stack joins")


def break_in_if_in_try():
    i = 0
    while True:
        try:
            i += 1
            if i > 2:
                break
        except ValueError:
            pass
    return i


print("break in if in try", break_in_if_in_try())


def continue_in_except_in_nested_try():
    events = []
    for value in (1, 2, 3):
        try:
            try:
                if value == 2:
                    raise ValueError("skip")
                events.append("body-" + str(value))
            except ValueError:
                events.append("skip-" + str(value))
                continue
            events.append("inner-done-" + str(value))
        except TypeError:
            events.append("outer-" + str(value))
        events.append("iter-done-" + str(value))
    return events


print("continue in except in nested try", continue_in_except_in_nested_try())


def return_through_finally_in_if():
    events = []

    def inner():
        try:
            if True:
                return "early"
            return "late"
        finally:
            events.append("finally ran")

    result = inner()
    return (result, events)


print("return through finally in if", return_through_finally_in_if())


def return_in_if_in_try_except():
    def inner():
        try:
            if True:
                return "early"
        except ValueError:
            return "caught"
        return "late"

    return inner()


print("return in if in try except", return_in_if_in_try_except())


def loop_control_through_finally():
    events = []
    for value in (1, 2, 3, 4):
        try:
            if value == 2:
                continue
            if value == 3:
                break
            events.append("body-" + str(value))
        finally:
            events.append("finally-" + str(value))
    events.append("after")
    return events


print("loop control through finally", loop_control_through_finally())


def while_else_completes_with_try():
    events = []
    n = 0
    while n < 3:
        try:
            n += 1
            if n == 5:
                break
        except ValueError:
            events.append("caught")
    else:
        events.append("while-else")
    events.append("n=" + str(n))
    return events


print("while else completes with try", while_else_completes_with_try())


def while_else_break_through_try():
    events = []
    n = 0
    while True:
        try:
            n += 1
            if n == 2:
                break
        finally:
            events.append("fin-" + str(n))
    else:
        events.append("unreachable-else")
    events.append("n=" + str(n))
    return events


print("while else break through try", while_else_break_through_try())


def break_across_two_trys():
    events = []
    for value in (1, 2):
        try:
            try:
                if value == 1:
                    events.append("break-now")
                    break
                events.append("unreachable-body")
            finally:
                events.append("inner-finally-" + str(value))
        finally:
            events.append("outer-finally-" + str(value))
    events.append("after")
    return events


print("break across two trys", break_across_two_trys())


def return_in_except_with_finally():
    events = []

    def inner():
        try:
            raise KeyError("boom")
        except KeyError:
            if events is not None:
                return "handled"
            return "unreachable"
        finally:
            events.append("cleanup")

    result = inner()
    return (result, events)


print("return in except with finally", return_in_except_with_finally())


def continue_in_else_clause():
    events = []
    for value in (1, 2, 3):
        try:
            events.append("try-" + str(value))
        except ValueError:
            events.append("caught-" + str(value))
        else:
            if value == 2:
                continue
        finally:
            events.append("finally-" + str(value))
        events.append("tail-" + str(value))
    return events


print("continue in else clause", continue_in_else_clause())


def nested_loop_break_stays_inside_try():
    # break targeting the inner loop does not leave the try: the handler
    # record stays active across the join after the inner loop.
    events = []
    try:
        for outer in (1, 2):
            for inner in (10, 20):
                if inner == 20:
                    break
                events.append(str(outer) + ":" + str(inner))
        events.append("loops-done")
    except ValueError:
        events.append("caught")
    return events


print("nested loop break stays inside try", nested_loop_break_stays_inside_try())


def raise_in_finally_copy_routes_outward():
    # An exception raised while a finally copy runs on a departing edge must
    # route to the ENCLOSING handler, not back into the departed try.
    events = []
    try:
        for value in (1, 2):
            try:
                if value == 1:
                    break
            finally:
                events.append("fin-" + str(value))
                raise RuntimeError("from finally")
    except RuntimeError as exc:
        events.append("caught: " + str(exc))
    return events


print("raise in finally copy routes outward", raise_in_finally_copy_routes_outward())
