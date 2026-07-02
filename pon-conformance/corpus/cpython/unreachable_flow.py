# Dead code after terminators, and loop-control edges that leave `with`
# bodies: CPython compiles unreachable statements without executing them,
# and `break`/`continue`/`return` departing a `with` runs `__exit__` first.


class CM:
    def __init__(self, tag):
        self.tag = tag

    def __enter__(self):
        print("enter", self.tag)
        return self

    def __exit__(self, exc_type, exc, tb):
        print("exit", self.tag)
        return False


# --- statements after return: simple ---
def after_return_simple():
    return "live"
    x = 1
    print("dead simple", x)
    return "dead"


print(after_return_simple())


# --- statements after return: in branches ---
def after_return_branches(flag):
    if flag:
        return "then"
        print("dead then")
    else:
        return "else"
        print("dead else")


print(after_return_branches(True), after_return_branches(False))


# --- statements after return/break/continue: in loops ---
def after_terminators_in_loop():
    out = []
    for i in range(5):
        if i == 0:
            continue
            out.append("dead continue")
        if i == 3:
            break
            out.append("dead break")
        out.append(i)
        if i == 99:
            return out
            out.append("dead return")
    return out


print(after_terminators_in_loop())


# --- the empty-generator idiom: yield after return / raise ---
def empty_gen():
    return
    yield


g = empty_gen()
print(list(g), type(g).__name__)


def raising_gen():
    raise KeyError("boom")
    yield 1


try:
    next(raising_gen())
except KeyError as exc:
    print("caught", exc)


# --- break/continue in try/finally inside loops ---
out = []
for i in range(4):
    try:
        if i == 1:
            continue
        if i == 3:
            break
        out.append(i)
    finally:
        out.append(("fin", i))
print(out)

count = 0
while count < 5:
    count += 1
    try:
        if count == 2:
            continue
        if count == 4:
            break
    finally:
        print("fin-while", count)
print("count", count)

# --- continue in with-block in loop ---
for i in range(3):
    with CM(i):
        if i == 1:
            continue
        print("body", i)
print("with-continue done")

# --- break in with-block in loop: direct tail, multi-item, nested ---
counter = 0
while True:
    counter += 1
    with CM("brk"):
        counter += 10
        break
    counter += 100
print("counter", counter)

for i in range(3):
    with CM("outer"), CM("inner"):
        if i == 1:
            break
        print("multi body", i)
print("multi done")

for i in range(3):
    with CM("o"):
        with CM("n"):
            if i == 1:
                break
            print("nested body", i)
print("nested done")


# --- return leaving a with body runs __exit__ ---
def with_return(flag):
    with CM("ret"):
        if flag:
            return "early"
        print("tail", flag)
    return "late"


print(with_return(True))
print(with_return(False))


def with_return_from_loop():
    with CM("loop-ret"):
        for i in range(5):
            if i == 2:
                return i
    return -1


print(with_return_from_loop())


# --- loop in a match arm; break in a match arm inside a loop ---
def match_loops(subject):
    out = []
    match subject:
        case "loop":
            for i in range(3):
                if i == 2:
                    break
                out.append(i)
        case _:
            out.append("other")
    return out


print(match_loops("loop"), match_loops("nope"))

for i in range(4):
    match i:
        case 2:
            break
        case _:
            print("match pass", i)
print("match-in-loop done")
