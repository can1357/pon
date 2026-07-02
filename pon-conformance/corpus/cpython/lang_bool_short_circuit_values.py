# Derived from CPython v3.14.0 Lib/test/test_bool.py topics (PSF license).

events = []


class Flag:
    def __init__(self, name, truth):
        self.name = name
        self.truth = truth

    def __bool__(self):
        events.append("bool " + self.name)
        return self.truth


def show(label, value):
    print(label, value.name)


false_flag = Flag("false", False)
true_flag = Flag("true", True)
right_flag = Flag("right", True)
other_flag = Flag("other", False)

show("and false", false_flag and right_flag)
show("and true", true_flag and right_flag)
show("or true", true_flag or right_flag)
show("or false", false_flag or right_flag)
show("mixed", true_flag and other_flag or right_flag)
print("events", events)

choice_one = "yes" if (true_flag and right_flag) else "no"
choice_two = "yes" if (false_flag or other_flag) else "no"
print("ternary", choice_one, choice_two)
print("events2", events)
