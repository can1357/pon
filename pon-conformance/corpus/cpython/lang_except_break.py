print("except break control")


def break_in_except():
    events = []
    for value in (1, 2):
        try:
            raise ValueError("stop")
        except ValueError:
            events.append("except-" + str(value))
            break
        events.append("after")
    else:
        events.append("for-else")
    events.append("done")
    return events


print("break in except", break_in_except())
